use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    BuyCexSellDex,
    BuyDexSellCex,
}

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

    pub fn age_seconds(&self) -> f64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        now - self.timestamp
    }
}
