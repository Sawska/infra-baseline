use crate::signal::Signal;
use inventory::tracker::InventoryTracker;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tokio::sync::Mutex;

/// Configuration for the opportunity scoring logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScorerConfig {
    pub spread_weight: f64,
    pub liquidity_weight: f64,
    pub inventory_weight: f64,
    pub history_weight: f64,
    pub excellent_spread_bps: f64,
    pub min_spread_bps: f64,
}

impl Default for ScorerConfig {
    fn default() -> Self {
        Self {
            spread_weight: 0.4,
            liquidity_weight: 0.2,
            inventory_weight: 0.2,
            history_weight: 0.2,
            excellent_spread_bps: 100.0,
            min_spread_bps: 30.0,
        }
    }
}

pub struct SignalScorer {
    config: ScorerConfig,
    /// Stores the last 100 trade results: (pair, success)
    recent_results: VecDeque<(String, bool)>,
}

impl SignalScorer {
    pub fn new(config: Option<ScorerConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
            recent_results: VecDeque::with_capacity(100),
        }
    }

    /// Calculates a score from 0 to 100 for a given signal.
    pub async fn score(&self, signal: &Signal, inventory: &Mutex<InventoryTracker>) -> f64 {
        let inventory_guard = inventory.lock().await;
        let spread_score = self.score_spread(signal.spread_bps);
        let liquidity_score = 80.0;
        let inventory_score = self.score_inventory(signal, &inventory_guard);
        let history_score = self.score_history(&signal.pair);

        let weighted = (spread_score * self.config.spread_weight)
            + (liquidity_score * self.config.liquidity_weight)
            + (inventory_score * self.config.inventory_weight)
            + (history_score * self.config.history_weight);

        (weighted.clamp(0.0, 100.0) * 10.0).round() / 10.0
    }

    fn score_spread(&self, spread_bps: f64) -> f64 {
        if spread_bps <= self.config.min_spread_bps {
            return 0.0;
        }
        if spread_bps >= self.config.excellent_spread_bps {
            return 100.0;
        }
        let range = self.config.excellent_spread_bps - self.config.min_spread_bps;
        ((spread_bps - self.config.min_spread_bps) / range) * 100.0
    }

    fn score_inventory(&self, signal: &Signal, inventory: &InventoryTracker) -> f64 {
        let assets: Vec<&str> = signal.pair.split('/').collect();
        let base = assets.first().unwrap_or(&"");

        let skew_data = inventory.skew(base);
        let needs_rebalance = skew_data["needs_rebalance"].as_bool().unwrap_or(false);

        if needs_rebalance { 20.0 } else { 60.0 }
    }

    fn score_history(&self, pair: &str) -> f64 {
        let pair_results: Vec<bool> = self
            .recent_results
            .iter()
            .filter(|(p, _)| p == pair)
            .take(20)
            .map(|(_, success)| *success)
            .collect();

        if pair_results.len() < 3 {
            return 50.0;
        }

        let successes = pair_results.iter().filter(|&&s| s).count();
        (successes as f64 / pair_results.len() as f64) * 100.0
    }

    /// Records the outcome of an execution attempt.
    pub fn record_result(&mut self, pair: String, success: bool) {
        if self.recent_results.len() >= 100 {
            self.recent_results.pop_front();
        }
        self.recent_results.push_back((pair, success));
    }

    /// Applies time-based decay to a score as the signal approaches expiry.
    pub fn apply_decay(&self, signal: &Signal) -> f64 {
        let age = signal.age_seconds();
        let ttl = signal.expiry - signal.timestamp;

        if ttl <= 0.0 {
            return 0.0;
        }

        let decay_factor = (1.0 - (age / ttl) * 0.5).max(0.0);
        signal.score * decay_factor
    }
}
