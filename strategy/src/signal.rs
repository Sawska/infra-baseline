use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Represents the direction of the arbitrage trade between CEX and DEX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    BuyCexSellDex,
    BuyDexSellCex,
}

/// A validated arbitrage opportunity ready for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub signal_id: String,
    pub pair: String,
    pub direction: Direction,

    pub cex_price: f64,
    pub dex_price: f64,
    pub spread_bps: f64,
    pub size: f64,

    pub expected_gross_pnl: f64,
    pub expected_fees: f64,
    pub expected_net_pnl: f64,

    pub score: f64,
    pub timestamp: f64,
    pub expiry: f64,

    pub inventory_ok: bool,
    pub within_limits: bool,
}

impl Signal {
    /// Creates a new Signal instance with a unique ID and current timestamp.
    ///
    /// # Arguments
    /// * `pair` - The trading pair (e.g., "BTC/USDT")
    /// * `direction` - The trade direction
    /// * `cex_price` - Current price on the CEX
    /// * `dex_price` - Current price on the DEX
    /// * `spread_bps` - Calculated spread in basis points
    /// * `size` - Opportunity size
    /// * `expected_gross_pnl` - PnL before fees
    /// * `expected_fees` - Estimated execution fees
    /// * `expected_net_pnl` - PnL after fees
    /// * `score` - Calculated strategy score
    /// * `expiry` - Timestamp when this signal becomes invalid
    /// * `inventory_ok` - Whether required inventory is available
    /// * `within_limits` - Whether the trade respects risk limits
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pair: String,
        direction: Direction,
        cex_price: f64,
        dex_price: f64,
        spread_bps: f64,
        size: f64,
        expected_gross_pnl: f64,
        expected_fees: f64,
        expected_net_pnl: f64,
        score: f64,
        expiry: f64,
        inventory_ok: bool,
        within_limits: bool,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs_f64();

        let short_uuid = Uuid::new_v4().to_string()[..8].to_string();
        let signal_id = format!("{}_{}", pair.replace('/', ""), short_uuid);

        Self {
            signal_id,
            pair,
            direction,
            cex_price,
            dex_price,
            spread_bps,
            size,
            expected_gross_pnl,
            expected_fees,
            expected_net_pnl,
            score,
            timestamp: now,
            expiry,
            inventory_ok,
            within_limits,
        }
    }

    /// Validates if the signal is still actionable.
    pub fn is_valid(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        now < self.expiry
            && self.inventory_ok
            && self.within_limits
            && self.expected_net_pnl > 0.0
            && self.score > 0.0
    }

    /// Returns the age of the signal in seconds.
    pub fn age_seconds(&self) -> f64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        now - self.timestamp
    }
}
