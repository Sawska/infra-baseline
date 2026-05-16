use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeStructure {
    pub cex_taker_bps: f64,
    pub dex_swap_bps: f64,
    pub gas_cost_usd: f64,
}

impl Default for FeeStructure {
    fn default() -> Self {
        Self {
            cex_taker_bps: 10.0,
            dex_swap_bps: 1.0,
            gas_cost_usd: 0.02,
        }
    }
}

impl FeeStructure {
    pub fn total_fee_bps(&self, trade_value_usd: f64) -> f64 {
        if trade_value_usd <= 0.0 {
            return f64::INFINITY;
        }

        let gas_bps = (self.gas_cost_usd / trade_value_usd) * 10_000.0;
        self.cex_taker_bps + self.dex_swap_bps + gas_bps
    }

    pub fn breakeven_spread_bps(&self, trade_value_usd: f64) -> f64 {
        self.total_fee_bps(trade_value_usd)
    }

    pub fn net_profit_usd(&self, spread_bps: f64, trade_value_usd: f64) -> f64 {
        let gross_pnl = (spread_bps / 10_000.0) * trade_value_usd;
        let total_fees = (self.total_fee_bps(trade_value_usd) / 10_000.0) * trade_value_usd;

        gross_pnl - total_fees
    }
}
