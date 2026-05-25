use crate::rebalancer::{TRANSFER_FEES, TransferPlan};
use crate::tracker::{InventoryTracker, Venue};
use alloy_primitives::{Address, U256};
use pricing::amm::{Pool, Token};
use pricing::monitor::{MonitorEvent, ParsedSwap};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub id: String,
    pub required_capital: HashMap<Venue, HashMap<String, Decimal>>,
    pub expected_profit_usd: Decimal,
    pub ttl: Duration,
    pub discovered_at: Instant,
}

impl ArbOpportunity {
    pub fn is_expired(&self) -> bool {
        self.discovered_at.elapsed() > self.ttl
    }

    pub fn shortfall(
        &self,
        tracker: &InventoryTracker,
    ) -> HashMap<Venue, HashMap<String, Decimal>> {
        let mut result = HashMap::new();
        for (venue, assets) in &self.required_capital {
            for (asset, &required) in assets {
                let available = tracker.get_available(*venue, asset);
                if available < required {
                    result
                        .entry(*venue)
                        .or_insert_with(HashMap::new)
                        .insert(asset.clone(), required - available);
                }
            }
        }
        result
    }
}

#[derive(Debug, Clone)]
pub struct PredictiveConfig {
    pub min_profit_to_cost_ratio: Decimal,
    pub trigger_swap_size_usd: Decimal,
    pub max_preposition_wait: Duration,
    pub max_queue_size: usize,
    pub safety_buffer_pct: Decimal,
}

impl Default for PredictiveConfig {
    fn default() -> Self {
        Self {
            min_profit_to_cost_ratio: dec!(3.0),
            trigger_swap_size_usd: dec!(50000),
            max_preposition_wait: Duration::from_secs(30),
            max_queue_size: 20,
            safety_buffer_pct: dec!(0.05),
        }
    }
}

#[derive(Debug)]
pub struct PrepositionPlan {
    pub opportunity_id: String,
    pub transfers: Vec<TransferPlan>,
    pub estimated_cost_usd: Decimal,
    pub expected_profit_usd: Decimal,
    pub should_execute: bool,
    pub reason: String,
}

pub struct PredictiveRebalancer {
    pub tracker: Arc<Mutex<InventoryTracker>>,
    config: PredictiveConfig,
    opportunity_queue: Arc<RwLock<Vec<ArbOpportunity>>>,
    prices_usd: Arc<RwLock<HashMap<String, Decimal>>>,
    pub address_to_token: HashMap<Address, Token>,
    pub pools: HashMap<String, Pool>,
    pub weth_address: Address,
}

impl PredictiveRebalancer {
    pub fn new(
        tracker: Arc<Mutex<InventoryTracker>>,
        config: PredictiveConfig,
        address_to_token: HashMap<Address, Token>,
        pools: HashMap<String, Pool>,
        weth_address: Address,
    ) -> Self {
        Self {
            tracker,
            config,
            opportunity_queue: Arc::new(RwLock::new(Vec::new())),
            prices_usd: Arc::new(RwLock::new(HashMap::new())),
            address_to_token,
            pools,
            weth_address,
        }
    }

    pub async fn update_price(&self, asset: &str, price_usd: Decimal) {
        self.prices_usd
            .write()
            .await
            .insert(asset.to_string(), price_usd);
    }

    async fn price_of(&self, asset: &str) -> Decimal {
        self.prices_usd
            .read()
            .await
            .get(asset)
            .cloned()
            .unwrap_or(Decimal::ONE)
    }

    pub async fn push_opportunity(&self, opp: ArbOpportunity) {
        let mut q = self.opportunity_queue.write().await;
        q.retain(|o| !o.is_expired());
        if q.len() < self.config.max_queue_size {
            q.push(opp);
        }
    }

    pub async fn pop_opportunity(&self, id: &str) -> Option<ArbOpportunity> {
        let mut q = self.opportunity_queue.write().await;
        if let Some(pos) = q.iter().position(|o| o.id == id) {
            Some(q.remove(pos))
        } else {
            None
        }
    }

    pub async fn on_monitor_event(&self, event: &MonitorEvent) -> Option<PrepositionPlan> {
        match event {
            MonitorEvent::MempoolSwap(swap) => self.on_pending_swap(swap).await,
            MonitorEvent::PoolUpdate { .. } => None,
        }
    }

    async fn on_pending_swap(&self, swap: &ParsedSwap) -> Option<PrepositionPlan> {
        let (token_in, token_out) = self.assets_from_swap(swap)?;

        let amount_in_decimal = u256_to_decimal(swap.amount_in, token_in.decimals);
        let price_in = self.price_of(&token_in.symbol).await;
        let swap_size_usd = amount_in_decimal * price_in;

        if swap_size_usd < self.config.trigger_swap_size_usd {
            return None;
        }

        let pair_fwd = format!("{}/{}", token_in.symbol, token_out.symbol);
        let pair_rev = format!("{}/{}", token_out.symbol, token_in.symbol);
        let pool = self
            .pools
            .get(&pair_fwd)
            .or_else(|| self.pools.get(&pair_rev))?;

        let estimated_arb = self
            .estimate_arb_from_swap(
                pool,
                &token_in,
                &token_out,
                swap.amount_in,
                amount_in_decimal,
                swap_size_usd,
            )
            .await;

        self.plan_for_opportunity(estimated_arb).await
    }

    pub async fn plan_for_opportunity(&self, opp: ArbOpportunity) -> Option<PrepositionPlan> {
        if opp.is_expired() {
            return Some(PrepositionPlan {
                opportunity_id: opp.id,
                transfers: vec![],
                estimated_cost_usd: Decimal::ZERO,
                expected_profit_usd: opp.expected_profit_usd,
                should_execute: false,
                reason: "Opportunity expired before planning".to_string(),
            });
        }

        let shortfall = {
            let tracker = self.tracker.lock().await;
            opp.shortfall(&tracker)
        };

        if shortfall.is_empty() {
            return Some(PrepositionPlan {
                opportunity_id: opp.id.clone(),
                transfers: vec![],
                estimated_cost_usd: Decimal::ZERO,
                expected_profit_usd: opp.expected_profit_usd,
                should_execute: true,
                reason: "Sufficient capital at all venues, no transfer needed".to_string(),
            });
        }

        let transfers = self.build_transfers_for_shortfall(&shortfall).await;

        let cost_usd = self.estimate_transfer_cost_usd(&transfers).await;

        let ratio = if cost_usd > Decimal::ZERO {
            opp.expected_profit_usd / cost_usd
        } else {
            Decimal::MAX
        };

        let should_execute = ratio >= self.config.min_profit_to_cost_ratio;
        let reason = if should_execute {
            format!(
                "Profit/cost ratio {:.2}x >= threshold {:.2}x",
                ratio, self.config.min_profit_to_cost_ratio
            )
        } else {
            format!(
                "Profit/cost ratio {:.2}x < threshold {:.2}x — skipping",
                ratio, self.config.min_profit_to_cost_ratio
            )
        };

        Some(PrepositionPlan {
            opportunity_id: opp.id,
            transfers,
            estimated_cost_usd: cost_usd,
            expected_profit_usd: opp.expected_profit_usd,
            should_execute,
            reason,
        })
    }

    pub async fn transfers_needed_for(&self, opp: &ArbOpportunity) -> (bool, Vec<TransferPlan>) {
        let shortfall = {
            let tracker = self.tracker.lock().await;
            opp.shortfall(&tracker)
        };
        if shortfall.is_empty() {
            return (true, vec![]);
        }
        let transfers = self.build_transfers_for_shortfall(&shortfall).await;
        let has_plan = !transfers.is_empty();
        (has_plan, transfers)
    }

    async fn build_transfers_for_shortfall(
        &self,
        shortfall: &HashMap<Venue, HashMap<String, Decimal>>,
    ) -> Vec<TransferPlan> {
        let mut plans = Vec::new();

        for (target_venue, assets) in shortfall {
            for (asset, &needed) in assets {
                let source_venue = match target_venue {
                    Venue::Wallet => Venue::Cex,
                    Venue::Cex => Venue::Wallet,
                };

                let available_at_source = {
                    let tracker = self.tracker.lock().await;
                    tracker.get_available(source_venue, asset)
                };

                let max_transferable =
                    available_at_source * (Decimal::ONE - self.config.safety_buffer_pct);

                if max_transferable <= Decimal::ZERO {
                    continue;
                }

                let amount = needed.min(max_transferable);

                if let Some(cfg) = TRANSFER_FEES.get(asset.as_str()) {
                    if amount < cfg.min_withdrawal {
                        continue;
                    }

                    plans.push(TransferPlan {
                        from_venue: source_venue,
                        to_venue: *target_venue,
                        asset: asset.clone(),
                        amount,
                        estimated_fee: cfg.withdrawal_fee,
                        estimated_time_min: cfg.estimated_time_min,
                    });
                }
            }
        }

        plans
    }

    async fn estimate_transfer_cost_usd(&self, plans: &[TransferPlan]) -> Decimal {
        let mut total = Decimal::ZERO;
        for plan in plans {
            let price = self.price_of(&plan.asset).await;
            total += plan.estimated_fee * price;
        }
        total
    }

    fn assets_from_swap(&self, swap: &ParsedSwap) -> Option<(Token, Token)> {
        let resolve = |addr_opt: Option<Address>| -> Option<Token> {
            match addr_opt {
                Some(addr) => self.address_to_token.get(&addr).cloned(),
                None => self.address_to_token.get(&self.weth_address).cloned(),
            }
        };

        let token_in = resolve(swap.token_in)?;
        let token_out = resolve(swap.token_out)?;
        Some((token_in, token_out))
    }

    async fn estimate_arb_from_swap(
        &self,
        pool: &Pool,
        token_in: &Token,
        token_out: &Token,
        amount_in: U256,
        amount_in_decimal: Decimal,
        swap_size_usd: Decimal,
    ) -> ArbOpportunity {
        let price_impact_pct = pool
            .get_price_impact(amount_in, token_in)
            .unwrap_or(Decimal::ZERO);

        let gross_profit_usd = swap_size_usd * price_impact_pct * dec!(0.5);

        let half = amount_in_decimal / dec!(2);
        let mut required_capital = HashMap::new();

        let mut wallet_needs = HashMap::new();
        wallet_needs.insert(token_out.symbol.clone(), half);
        required_capital.insert(Venue::Wallet, wallet_needs);

        let mut cex_needs = HashMap::new();
        cex_needs.insert(token_in.symbol.clone(), half);
        required_capital.insert(Venue::Cex, cex_needs);

        ArbOpportunity {
            id: format!("arb-{}", uuid_simple()),
            required_capital,
            expected_profit_usd: gross_profit_usd,
            ttl: Duration::from_secs(12),
            discovered_at: Instant::now(),
        }
    }
}

fn u256_to_decimal(val: U256, decimals: u8) -> Decimal {
    let divisor = Decimal::from(10u64.pow(decimals as u32));
    let as_u128 = val.wrapping_to::<u128>();
    Decimal::from(as_u128) / divisor
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", t)
}
