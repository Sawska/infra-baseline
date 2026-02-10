use crate::fees::FeeStructure;
use crate::signal::{Direction, Signal};
use alloy_primitives::U256;
use exchange::client::{ExchangeClient, OrderBook};
use inventory::tracker::{InventoryTracker, Venue};
use pricing::amm::{Pool, Token};
use rust_decimal::prelude::*;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

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

pub struct SignalGenerator {
    exchange: ExchangeClient,
    pub inventory: InventoryTracker,
    fees: FeeStructure,
    pub config: GeneratorConfig,
    /// Maps pair strings (e.g., "ETH/USDT") to their AMM pool metadata
    pair_metadata: HashMap<String, PairMetadata>,
    last_signal_time: HashMap<String, f64>,
}

impl SignalGenerator {
    pub fn new(
        exchange: ExchangeClient,
        inventory: InventoryTracker,
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
        }
    }

    /// Registers a pool for a specific trading pair.
    pub fn register_pair(&mut self, pair: String, metadata: PairMetadata) {
        self.pair_metadata.insert(pair, metadata);
    }

    /// Attempt to generate a signal for the given pair and size.
    pub async fn generate(&mut self, pair: &str, size: f64) -> Option<Signal> {
        let now = self.get_now();

        if self.in_cooldown(pair, now) {
            return None;
        }

        let prices = self.fetch_prices(pair, size).await?;

        let spread_a = (prices.dex_sell - prices.cex_ask) / prices.cex_ask * 10_000.0;

        let spread_b = (prices.cex_bid - prices.dex_buy) / prices.dex_buy * 10_000.0;

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
                return None;
            };

        let trade_value = size * execution_cex;
        let gross_pnl = (spread / 10_000.0) * trade_value;
        let total_fees = (self.fees.total_fee_bps(trade_value) / 10_000.0) * trade_value;
        let net_pnl = gross_pnl - total_fees;

        if net_pnl < self.config.min_profit_usd {
            return None;
        }

        let inventory_ok = self.check_inventory(pair, direction, size, execution_cex);
        let within_limits = trade_value <= self.config.max_position_usd;

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

        let dex_sell = metadata
            .pool
            .get_execution_price(size_u256, &metadata.base_token)
            .ok()?
            .to_f64()?;

        let mid_price = (cex_bid + cex_ask) / 2.0;
        let quote_u256 = self.f64_to_u256(size * mid_price, metadata.quote_token.decimals);
        let base_out_per_quote_in = metadata
            .pool
            .get_execution_price(quote_u256, &metadata.quote_token)
            .ok()?
            .to_f64()?;
        let dex_buy = 1.0 / base_out_per_quote_in;

        Some(PriceData {
            cex_bid,
            cex_ask,
            dex_buy,
            dex_sell,
        })
    }

    fn check_inventory(&self, pair: &str, direction: Direction, size: f64, price: f64) -> bool {
        let assets: Vec<&str> = pair.split('/').collect();
        if assets.len() != 2 {
            return false;
        }
        let (base, quote) = (assets[0], assets[1]);

        match direction {
            Direction::BuyCexSellDex => {
                let quote_cex = self
                    .inventory
                    .get_available(Venue::Cex, quote)
                    .to_f64()
                    .unwrap_or(0.0);
                let base_dex = self
                    .inventory
                    .get_available(Venue::Wallet, base)
                    .to_f64()
                    .unwrap_or(0.0);

                quote_cex >= size * price * 1.01 && base_dex >= size
            }
            Direction::BuyDexSellCex => {
                let base_cex = self
                    .inventory
                    .get_available(Venue::Cex, base)
                    .to_f64()
                    .unwrap_or(0.0);
                let quote_dex = self
                    .inventory
                    .get_available(Venue::Wallet, quote)
                    .to_f64()
                    .unwrap_or(0.0);

                base_cex >= size && quote_dex >= size * price * 1.01
            }
        }
    }

    fn in_cooldown(&self, pair: &str, now: f64) -> bool {
        if let Some(&last_time) = self.last_signal_time.get(pair) {
            return now - last_time < self.config.cooldown_seconds;
        }
        false
    }

    fn get_now(&self) -> f64 {
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
}
