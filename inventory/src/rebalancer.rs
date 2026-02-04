use crate::tracker::{InventoryTracker, Venue};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

lazy_static::lazy_static! {
    pub static ref TRANSFER_FEES: HashMap<&'static str, TransferConfig> = {
        let mut m = HashMap::new();
        m.insert("ETH", TransferConfig {
            withdrawal_fee: dec!(0.005),
            min_withdrawal: dec!(0.01),
            estimated_time_min: 15,
        });
        m.insert("USDT", TransferConfig {
            withdrawal_fee: dec!(1.0),
            min_withdrawal: dec!(10.0),
            estimated_time_min: 15,
        });
        m.insert("USDC", TransferConfig {
            withdrawal_fee: dec!(1.0),
            min_withdrawal: dec!(10.0),
            estimated_time_min: 15,
        });
        m
    };

    pub static ref MIN_OPERATING_BALANCE: HashMap<&'static str, Decimal> = {
        let mut m = HashMap::new();
        m.insert("ETH", dec!(0.5));
        m.insert("USDT", dec!(500));
        m.insert("USDC", dec!(500));
        m
    };
}

#[derive(Debug, Clone)]
pub struct TransferConfig {
    pub withdrawal_fee: Decimal,
    pub min_withdrawal: Decimal,
    pub estimated_time_min: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferPlan {
    pub from_venue: Venue,
    pub to_venue: Venue,
    pub asset: String,
    pub amount: Decimal,
    pub estimated_fee: Decimal,
    pub estimated_time_min: i32,
}

impl TransferPlan {
    pub fn net_amount(&self) -> Decimal {
        self.amount - self.estimated_fee
    }
}

pub struct RebalancePlanner<'a> {
    pub tracker: &'a InventoryTracker,
    pub threshold_pct: f64,
}

impl<'a> RebalancePlanner<'a> {
    pub fn new(tracker: &'a InventoryTracker, threshold_pct: f64) -> Self {
        Self {
            tracker,
            threshold_pct,
        }
    }

    /// Check all tracked assets for skew.
    pub fn check_all(&self) -> Vec<serde_json::Value> {
        let mut all_assets = std::collections::HashSet::new();
        for venue_balances in self.tracker.balances.values() {
            for asset in venue_balances.keys() {
                all_assets.insert(asset.clone());
            }
        }

        all_assets
            .into_iter()
            .map(|asset| self.tracker.skew(&asset))
            .collect()
    }

    /// Generate transfer plan to rebalance a specific asset.
    pub fn plan(&self, asset: &str) -> Vec<TransferPlan> {
        let skew = self.tracker.skew(asset);
        if !skew["needs_rebalance"].as_bool().unwrap_or(false) {
            return vec![];
        }

        let total =
            Decimal::from_str(skew["total"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO);
        let target_amount = total / dec!(2);

        let binance_amount = self.tracker.get_available(Venue::Binance, asset);
        let wallet_amount = self.tracker.get_available(Venue::Wallet, asset);

        let (from_venue, to_venue, amount_to_move) = if binance_amount > target_amount {
            (
                Venue::Binance,
                Venue::Wallet,
                binance_amount - target_amount,
            )
        } else {
            (Venue::Wallet, Venue::Binance, wallet_amount - target_amount)
        };

        let config = TRANSFER_FEES.get(asset);
        let min_op = MIN_OPERATING_BALANCE
            .get(asset)
            .cloned()
            .unwrap_or(Decimal::ZERO);

        if let Some(cfg) = config {
            if amount_to_move < cfg.min_withdrawal {
                return vec![];
            }

            let current_from_bal = self.tracker.get_available(from_venue, asset);
            if current_from_bal - amount_to_move < min_op {
                let adjusted_move = current_from_bal - min_op;
                if adjusted_move < cfg.min_withdrawal {
                    return vec![];
                }
                return vec![TransferPlan {
                    from_venue,
                    to_venue,
                    asset: asset.to_string(),
                    amount: adjusted_move,
                    estimated_fee: cfg.withdrawal_fee,
                    estimated_time_min: cfg.estimated_time_min,
                }];
            }

            return vec![TransferPlan {
                from_venue,
                to_venue,
                asset: asset.to_string(),
                amount: amount_to_move,
                estimated_fee: cfg.withdrawal_fee,
                estimated_time_min: cfg.estimated_time_min,
            }];
        }

        vec![]
    }

    pub fn plan_all(&self) -> HashMap<String, Vec<TransferPlan>> {
        let mut plans = HashMap::new();
        let skews = self.check_all();
        for skew in skews {
            let asset = skew["asset"].as_str().unwrap();
            let p = self.plan(asset);
            if !p.is_empty() {
                plans.insert(asset.to_string(), p);
            }
        }
        plans
    }

    pub fn estimate_cost(&self, plans: &[TransferPlan]) -> serde_json::Value {
        let mut total_transfers = 0;
        let mut total_fees: HashMap<String, Decimal> = HashMap::new();
        let mut max_time = 0;
        let mut assets_affected = Vec::new();

        for plan in plans {
            total_transfers += 1;
            let entry = total_fees
                .entry(plan.asset.clone())
                .or_insert(Decimal::ZERO);
            *entry += plan.estimated_fee;
            max_time = max_time.max(plan.estimated_time_min);
            if !assets_affected.contains(&plan.asset) {
                assets_affected.push(plan.asset.clone());
            }
        }

        serde_json::json!({
            "total_transfers": total_transfers,
            "total_fees": total_fees,
            "total_time_min": max_time,
            "assets_affected": assets_affected,
        })
    }
}
