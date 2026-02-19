use crate::fees::FeeStructure;
use crate::signal::{Direction, Signal};
use alloy_primitives::U256;
use exchange::client::{ExchangeClient, OrderBook};
use inventory::tracker::{InventoryTracker, Venue};
use log::{info, warn};
use pricing::amm::{Pool, Token};
use rust_decimal::prelude::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Configuration for the Signal Generator
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    pub min_spread_bps: f64,
    pub min_profit_usd: f64,
    pub max_position_usd: f64,
    pub signal_ttl_seconds: f64,
    pub cooldown_seconds: f64,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            min_spread_bps: 50.0,
            min_profit_usd: 5.0,
            max_position_usd: 10_000.0,
            signal_ttl_seconds: 5.0,
            cooldown_seconds: 2.0,
        }
    }
}

/// Metadata required to link a trading pair to on-chain AMM pools
pub struct PairMetadata {
    pub pool: Pool,
    pub base_token: Token,
    pub quote_token: Token,
}

/// Container for price data across venues
#[derive(Debug, Clone)]
struct PriceData {
    cex_bid: f64,
    cex_ask: f64,
    dex_buy: f64,
    dex_sell: f64,
}

/// Wrapper to implement Ord for Signal based on Net PnL
#[derive(Debug)]
struct PrioritizedSignal(Signal);

impl PartialEq for PrioritizedSignal {
    fn eq(&self, other: &Self) -> bool {
        self.0.expected_net_pnl == other.0.expected_net_pnl
    }
}

impl Eq for PrioritizedSignal {}

impl Ord for PrioritizedSignal {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.expected_net_pnl.total_cmp(&other.0.expected_net_pnl)
    }
}

impl PartialOrd for PrioritizedSignal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct SignalGenerator {
    pub exchange: Arc<ExchangeClient>,
    pub inventory: Arc<Mutex<InventoryTracker>>,
    fees: FeeStructure,
    pub config: GeneratorConfig,
    /// Maps pair strings (e.g., "ETH/USDT") to their AMM pool metadata
    pair_metadata: HashMap<String, PairMetadata>,
    last_signal_time: HashMap<String, f64>,
    /// Queue for signals sorted by priority (Net PnL)
    signal_queue: BinaryHeap<PrioritizedSignal>,
}

impl SignalGenerator {
    pub fn new(
        exchange: Arc<ExchangeClient>,
        inventory: Arc<Mutex<InventoryTracker>>,
        fees: FeeStructure,
        config: GeneratorConfig,
    ) -> Self {
        Self {
            exchange,
            inventory,
            fees,
            config,
            pair_metadata: HashMap::new(),
            last_signal_time: HashMap::new(),
            signal_queue: BinaryHeap::new(),
        }
    }

    /// Registers a pool for a specific trading pair.
    pub fn register_pair(&mut self, pair: String, metadata: PairMetadata) {
        self.pair_metadata.insert(pair, metadata);
    }

    /// Evaluates a pair and, if a valid signal is generated, adds it to the priority queue.
    pub async fn queue_candidate(&mut self, pair: &str, size: f64) {
        if let Some(signal) = self.generate(pair, size).await {
            self.signal_queue.push(PrioritizedSignal(signal));
        }
    }

    /// Returns the highest priority (highest PnL) signal from the queue.
    pub fn pop_top_signal(&mut self) -> Option<Signal> {
        self.signal_queue.pop().map(|wrapper| wrapper.0)
    }

    /// Clears the current signal queue.
    pub fn clear_queue(&mut self) {
        self.signal_queue.clear();
    }

    /// Attempt to generate a signal for the given pair and size.
    pub async fn generate(&mut self, pair: &str, size: f64) -> Option<Signal> {
        let now = SignalGenerator::get_now();

        if self.in_cooldown(pair, now) {
            info!("Skipping {} due to cooldown.", pair);
            return None;
        }
        let prices = match self.fetch_prices(pair, size).await {
            Some(p) => {
                info!("{} Prices | CEX: {} | DEX: {}", pair, p.cex_ask, p.dex_buy);
                p
            }
            None => {
                warn!(
                    "Failed to fetch prices for {}. (Possible symbol mismatch or API error)",
                    pair
                );
                return None;
            }
        };

        let spread_a = (prices.dex_sell - prices.cex_ask) / prices.cex_ask * 10_000.0;
        let spread_b = (prices.cex_bid - prices.dex_buy) / prices.dex_buy * 10_000.0;

        info!(
            "📊{} Spreads | BuyCex->SellDex: {:.2} bps | BuyDex->SellCex: {:.2} bps | Min Required: {:.2}",
            pair, spread_a, spread_b, self.config.min_spread_bps
        );

        let (direction, spread, execution_cex, execution_dex) =
            if spread_a > spread_b && spread_a >= self.config.min_spread_bps {
                (
                    Direction::BuyCexSellDex,
                    spread_a,
                    prices.cex_ask,
                    prices.dex_sell,
                )
            } else if spread_b >= self.config.min_spread_bps {
                (
                    Direction::BuyDexSellCex,
                    spread_b,
                    prices.cex_bid,
                    prices.dex_buy,
                )
            } else {
                info!("{} ignored: Spread too low.", pair);
                return None;
            };

        let trade_value = size * execution_cex;
        let gross_pnl = (spread / 10_000.0) * trade_value;
        let total_fees = (self.fees.total_fee_bps(trade_value) / 10_000.0) * trade_value;
        let net_pnl = gross_pnl - total_fees;

        info!(
            "{} Potential PnL: Gross ${:.4} - Fees ${:.4} = Net ${:.4} (Min: ${:.4})",
            pair, gross_pnl, total_fees, net_pnl, self.config.min_profit_usd
        );

        if net_pnl < self.config.min_profit_usd {
            warn!(
                "{} ignored: Not profitable enough (Net ${:.4})",
                pair, net_pnl
            );
            return None;
        }

        let inventory_ok = self
            .check_inventory(pair, direction, size, execution_cex)
            .await;
        let within_limits = trade_value <= self.config.max_position_usd;

        if !inventory_ok {
            warn!("{} inventory check failed (Insufficient funds?)", pair);
        }

        let signal = Signal::new(
            pair.to_string(),
            direction,
            execution_cex,
            execution_dex,
            spread,
            size,
            gross_pnl,
            total_fees,
            net_pnl,
            spread / 100.0,
            now + self.config.signal_ttl_seconds,
            inventory_ok,
            within_limits,
        );

        self.last_signal_time.insert(pair.to_string(), now);

        Some(signal)
    }
    /// Fetches the latest price data for both CEX and DEX.
    async fn fetch_prices(&self, pair: &str, size: f64) -> Option<PriceData> {
        let metadata = self.pair_metadata.get(pair)?;

        let ob: OrderBook = self.exchange.fetch_order_book(pair, 5).await.ok()?;
        let cex_bid = ob.best_bid.0.to_f64()?;
        let cex_ask = ob.best_ask.0.to_f64()?;

        let size_u256 = self.f64_to_u256(size, metadata.base_token.decimals);

        let mut dex_sell = metadata
            .pool
            .get_execution_price(size_u256, &metadata.base_token)
            .ok()?
            .to_f64()?;

        if dex_sell < 1.0 && cex_ask > 1000.0 {
            dex_sell = 1.0 / dex_sell;
        }

        let mid_price = (cex_bid + cex_ask) / 2.0;
        let quote_u256 = self.f64_to_u256(size * mid_price, metadata.quote_token.decimals);

        let base_out_per_quote_in = metadata
            .pool
            .get_execution_price(quote_u256, &metadata.quote_token)
            .ok()?
            .to_f64()?;

        let mut dex_buy = 1.0 / base_out_per_quote_in;

        if dex_buy < 1.0 && cex_bid > 1000.0 {
            dex_buy = 1.0 / dex_buy;
        }

        Some(PriceData {
            cex_bid,
            cex_ask,
            dex_buy,
            dex_sell,
        })
    }

    async fn check_inventory(
        &self,
        pair: &str,
        direction: Direction,
        size: f64,
        price: f64,
    ) -> bool {
        let assets: Vec<&str> = pair.split('/').collect();
        if assets.len() != 2 {
            return false;
        }
        let (base, quote) = (assets[0], assets[1]);
        let required_quote = size * price * 1.01;

        match direction {
            Direction::BuyCexSellDex => {
                let inventory = self.inventory.lock().await;
                let quote_cex = inventory
                    .get_available(Venue::Cex, quote)
                    .to_f64()
                    .unwrap_or(0.0);
                let base_dex = inventory
                    .get_available(Venue::Wallet, base)
                    .to_f64()
                    .unwrap_or(0.0);

                let has_quote = quote_cex >= required_quote;
                let has_base = base_dex >= size;

                if !has_quote || !has_base {
                    warn!(
                        "[Inventory] Insufficient funds for BuyCexSellDex on {}: \
                    CEX {} available: {:.2} (need {:.2}), \
                    DEX {} available: {:.2} (need {:.2})",
                        pair, quote, quote_cex, required_quote, base, base_dex, size
                    );
                }
                has_quote && has_base
            }
            Direction::BuyDexSellCex => {
                let inventory = self.inventory.lock().await;
                let base_cex = inventory
                    .get_available(Venue::Cex, base)
                    .to_f64()
                    .unwrap_or(0.0);
                let quote_dex = inventory
                    .get_available(Venue::Wallet, quote)
                    .to_f64()
                    .unwrap_or(0.0);

                let has_base = base_cex >= size;
                let has_quote = quote_dex >= required_quote;

                if !has_base || !has_quote {
                    warn!(
                        "[Inventory] Insufficient funds for BuyDexSellCex on {}: \
                    CEX {} available: {:.2} (need {:.2}), \
                    DEX {} available: {:.2} (need {:.2})",
                        pair, base, base_cex, size, quote, quote_dex, required_quote
                    );
                }
                has_base && has_quote
            }
        }
    }

    fn in_cooldown(&self, pair: &str, now: f64) -> bool {
        if let Some(&last_time) = self.last_signal_time.get(pair) {
            return now - last_time < self.config.cooldown_seconds;
        }
        false
    }

    pub fn get_now() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    fn f64_to_u256(&self, val: f64, decimals: u8) -> U256 {
        let factor = 10f64.powi(decimals as i32);
        let raw = (val * factor) as u128;
        U256::from(raw)
    }

    /// Dynamically updates the generator's core configuration (spreads, profit targets, etc.)
    pub fn update_config(&mut self, config: GeneratorConfig) {
        self.config = config;
    }

    /// Dynamically updates the fee structure (used when switching between V3 fee tiers)
    pub fn set_fees(&mut self, fees: FeeStructure) {
        self.fees = fees;
    }
}
