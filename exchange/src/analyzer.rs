use crate::client::OrderBook;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub price: Decimal,
    pub qty: Decimal,
    pub cost: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkResult {
    pub avg_price: Decimal,
    pub total_cost: Decimal,
    pub slippage_bps: Decimal,
    pub levels_consumed: usize,
    pub fully_filled: bool,
    pub fills: Vec<Fill>,
}

pub struct OrderBookAnalyzer {
    pub orderbook: OrderBook,
}

impl OrderBookAnalyzer {
    pub fn new(orderbook: OrderBook) -> Self {
        Self { orderbook }
    }

    pub fn walk_the_book(&self, side: &str, qty: Decimal) -> WalkResult {
        let levels = if side.to_lowercase() == "buy" {
            &self.orderbook.asks
        } else {
            &self.orderbook.bids
        };

        let best_price = if !levels.is_empty() {
            levels[0].0
        } else {
            Decimal::ZERO
        };
        let mut remaining_qty = qty;
        let mut total_cost = Decimal::ZERO;
        let mut actual_qty_filled = Decimal::ZERO;
        let mut levels_consumed = 0;
        let mut fills = Vec::new();

        for (price, level_qty) in levels {
            if remaining_qty <= Decimal::ZERO {
                break;
            }

            levels_consumed += 1;
            let take_qty = remaining_qty.min(*level_qty);
            let cost = take_qty * price;

            fills.push(Fill {
                price: *price,
                qty: take_qty,
                cost,
            });

            total_cost += cost;
            actual_qty_filled += take_qty;
            remaining_qty -= take_qty;
        }

        let fully_filled = remaining_qty <= Decimal::ZERO;
        let avg_price = if actual_qty_filled.is_zero() {
            Decimal::ZERO
        } else {
            total_cost / actual_qty_filled
        };

        let slippage_bps = if best_price.is_zero() || avg_price.is_zero() {
            Decimal::ZERO
        } else {
            let diff = (avg_price - best_price).abs();
            (diff / best_price) * Decimal::new(10000, 0)
        };

        WalkResult {
            avg_price: avg_price.normalize(),
            total_cost: total_cost.normalize(),
            slippage_bps: slippage_bps.round_dp(2),
            levels_consumed,
            fully_filled,
            fills,
        }
    }

    pub fn depth_at_bps(&self, side: &str, bps: Decimal) -> (Decimal, Decimal) {
        let levels = if side.to_lowercase() == "ask" {
            &self.orderbook.asks
        } else {
            &self.orderbook.bids
        };

        if levels.is_empty() {
            return (Decimal::ZERO, Decimal::ZERO);
        }

        let best_price = levels[0].0;
        let mut total_qty = Decimal::ZERO;
        let mut total_cost = Decimal::ZERO;

        for (price, qty) in levels {
            let price_diff_bps = if best_price.is_zero() {
                Decimal::MAX
            } else {
                ((price - best_price).abs() / best_price) * Decimal::new(10000, 0)
            };

            if price_diff_bps <= bps {
                total_qty += qty;
                total_cost += price * qty;
            } else {
                break;
            }
        }

        (total_qty.normalize(), total_cost.normalize())
    }

    pub fn imbalance(&self, levels: usize) -> Decimal {
        let bid_vol: Decimal = self.orderbook.bids.iter().take(levels).map(|l| l.1).sum();
        let ask_vol: Decimal = self.orderbook.asks.iter().take(levels).map(|l| l.1).sum();

        let total = bid_vol + ask_vol;
        if total.is_zero() {
            return Decimal::ZERO;
        }

        ((bid_vol - ask_vol) / total).round_dp(2)
    }

    pub fn effective_spread(&self, qty: Decimal) -> Decimal {
        let buy_walk = self.walk_the_book("buy", qty);
        let sell_walk = self.walk_the_book("sell", qty);

        if buy_walk.avg_price.is_zero()
            || sell_walk.avg_price.is_zero()
            || self.orderbook.mid_price.is_zero()
        {
            return Decimal::ZERO;
        }

        ((buy_walk.avg_price - sell_walk.avg_price) / self.orderbook.mid_price
            * Decimal::new(10000, 0))
        .round_dp(2)
    }
}
