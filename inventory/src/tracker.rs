use arb_core::{Address, TokenAmount, WalletManager};
use chrono::Utc;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Venue {
    Cex,
    Wallet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub venue: Venue,
    pub asset: String,
    pub free: Decimal,
    pub locked: Decimal,
}

impl Balance {
    pub fn total(&self) -> Decimal {
        self.free + self.locked
    }
}

pub struct InventoryTracker {
    /// Tracks which wallet address this tracker is associated with
    pub wallet_address: Option<Address>,
    /// Venue -> Asset Symbol -> Balance
    pub balances: HashMap<Venue, HashMap<String, Balance>>,
}

impl InventoryTracker {
    pub fn new(wallet: Option<&WalletManager>) -> Self {
        let mut venues = HashMap::new();
        venues.insert(Venue::Cex, HashMap::new());
        venues.insert(Venue::Wallet, HashMap::new());

        Self {
            wallet_address: wallet.map(|w| w.address()),
            balances: venues,
        }
    }

    pub fn update_from_cex(
        &mut self,
        venue: Venue,
        cex_balances: HashMap<String, (Decimal, Decimal)>,
    ) {
        if let Some(venue_map) = self.balances.get_mut(&venue) {
            venue_map.clear();
            for (asset, (free, locked)) in cex_balances {
                venue_map.insert(
                    asset.clone(),
                    Balance {
                        venue,
                        asset,
                        free,
                        locked,
                    },
                );
            }
        }
    }

    /// Update from Wallet (DEX) using your core::TokenAmount model
    /// This handles the conversion from U256 to Decimal automatically
    pub fn update_from_wallet(&mut self, wallet_balances: Vec<TokenAmount>) {
        if let Some(venue_map) = self.balances.get_mut(&Venue::Wallet) {
            venue_map.clear();
            for amount in wallet_balances {
                if let Some(symbol) = &amount.symbol {
                    venue_map.insert(
                        symbol.clone(),
                        Balance {
                            venue: Venue::Wallet,
                            asset: symbol.clone(),
                            free: amount.to_human(),
                            locked: Decimal::ZERO,
                        },
                    );
                }
            }
        }
    }

    /// Full portfolio snapshot
    pub fn snapshot(&self) -> serde_json::Value {
        let mut totals: HashMap<String, Decimal> = HashMap::new();

        for venue_balances in self.balances.values() {
            for (asset, balance) in venue_balances {
                let entry = totals.entry(asset.clone()).or_insert(Decimal::ZERO);
                *entry += balance.total();
            }
        }

        serde_json::json!({
            "timestamp": Utc::now(),
            "wallet": self.wallet_address.as_ref().map(|a| a.checksum()),
            "venues": self.balances,
            "totals": totals,
        })
    }

    pub fn get_available(&self, venue: Venue, asset: &str) -> Decimal {
        self.balances
            .get(&venue)
            .and_then(|v| v.get(asset))
            .map(|b| b.free)
            .unwrap_or(Decimal::ZERO)
    }

    /// Pre-flight check before executing an arbitrage trade
    pub fn can_execute(
        &self,
        buy_venue: Venue,
        buy_asset: &str,
        buy_amount: Decimal,
        sell_venue: Venue,
        sell_asset: &str,
        sell_amount: Decimal,
    ) -> serde_json::Value {
        let buy_available = self.get_available(buy_venue, buy_asset);
        let sell_available = self.get_available(sell_venue, sell_asset);

        let can_buy = buy_available >= buy_amount;
        let can_sell = sell_available >= sell_amount;

        let reason = if !can_buy {
            Some(format!("Insufficient {} on {:?}", buy_asset, buy_venue))
        } else if !can_sell {
            Some(format!("Insufficient {} on {:?}", sell_asset, sell_venue))
        } else {
            None
        };

        serde_json::json!({
            "can_execute": can_buy && can_sell,
            "buy_venue_available": buy_available,
            "buy_venue_needed": buy_amount,
            "sell_venue_available": sell_available,
            "sell_venue_needed": sell_amount,
            "reason": reason
        })
    }

    /// Record a trade to update local state immediately (optimistic update)
    #[allow(clippy::too_many_arguments)]
    pub fn record_trade(
        &mut self,
        venue: Venue,
        side: &str,
        base_asset: &str,
        quote_asset: &str,
        base_amount: Decimal,
        quote_amount: Decimal,
        fee: Decimal,
        fee_asset: &str,
    ) {
        if let Some(venue_map) = self.balances.get_mut(&venue) {
            let base = venue_map.entry(base_asset.to_string()).or_insert(Balance {
                venue,
                asset: base_asset.to_string(),
                free: Decimal::ZERO,
                locked: Decimal::ZERO,
            });
            if side == "buy" {
                base.free += base_amount;
            } else {
                base.free -= base_amount;
            }

            let quote = venue_map.entry(quote_asset.to_string()).or_insert(Balance {
                venue,
                asset: quote_asset.to_string(),
                free: Decimal::ZERO,
                locked: Decimal::ZERO,
            });
            if side == "buy" {
                quote.free -= quote_amount;
            } else {
                quote.free += quote_amount;
            }

            let fee_entry = venue_map.entry(fee_asset.to_string()).or_insert(Balance {
                venue,
                asset: fee_asset.to_string(),
                free: Decimal::ZERO,
                locked: Decimal::ZERO,
            });
            fee_entry.free -= fee;
        }
    }

    /// Calculate skew across CEX and DEX for rebalancing
    pub fn skew(&self, asset: &str) -> serde_json::Value {
        let mut total = Decimal::ZERO;
        let mut venue_amounts = HashMap::new();

        for (venue, assets) in &self.balances {
            let amount = assets
                .get(asset)
                .map(|b| b.total())
                .unwrap_or(Decimal::ZERO);
            total += amount;
            venue_amounts.insert(format!("{:?}", venue), amount);
        }

        let mut venue_analysis = HashMap::new();
        for (name, amount) in venue_amounts {
            let pct = if total.is_zero() {
                0.0
            } else {
                (amount / total).to_f64().unwrap_or(0.0)
            };
            venue_analysis.insert(
                name,
                serde_json::json!({
                    "amount": amount,
                    "pct": pct,
                    "deviation_pct": (pct - 0.5).abs()
                }),
            );
        }

        let max_dev = venue_analysis
            .values()
            .map(|v| v["deviation_pct"].as_f64().unwrap_or(0.0))
            .fold(0.0, f64::max);

        serde_json::json!({
            "asset": asset,
            "total": total,
            "venues": venue_analysis,
            "max_deviation_pct": max_dev,
            "needs_rebalance": max_dev > 0.30
        })
    }
}
