use serde::{Deserialize, Serialize};

/// Represents the fee configuration for arbitrage trades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeStructure {
    /// Centralized Exchange taker fee in basis points (default: 10.0 bps)
    pub cex_taker_bps: f64,
    /// Decentralized Exchange swap fee in basis points (default: 30.0 bps)
    pub dex_swap_bps: f64,
    /// Estimated fixed gas cost for the DEX transaction in USD
    pub gas_cost_usd: f64,
}

impl Default for FeeStructure {
    fn default() -> Self {
        Self {
            cex_taker_bps: 10.0,
            dex_swap_bps: 30.0,
            gas_cost_usd: 5.0,
        }
    }
}

impl FeeStructure {
    /// Calculates the total effective fee in basis points for a given trade size.
    /// Includes the variable exchange fees and the variable impact of fixed gas costs.
    pub fn total_fee_bps(&self, trade_value_usd: f64) -> f64 {
        if trade_value_usd <= 0.0 {
            return f64::INFINITY;
        }

        let gas_bps = (self.gas_cost_usd / trade_value_usd) * 10_000.0;
        self.cex_taker_bps + self.dex_swap_bps + gas_bps
    }

    /// Returns the minimum spread required in basis points to cover all costs.
    pub fn breakeven_spread_bps(&self, trade_value_usd: f64) -> f64 {
        self.total_fee_bps(trade_value_usd)
    }

    /// Calculates the expected net profit in USD after deducting all fees.
    pub fn net_profit_usd(&self, spread_bps: f64, trade_value_usd: f64) -> f64 {
        let gross_pnl = (spread_bps / 10_000.0) * trade_value_usd;
        let total_fees = (self.total_fee_bps(trade_value_usd) / 10_000.0) * trade_value_usd;

        gross_pnl - total_fees
    }
}
