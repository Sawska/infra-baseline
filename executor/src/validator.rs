use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use strategy::signal::Signal;

/// Sanity checks before every trade to prevent execution on bad data.
pub struct PreTradeValidator {
    /// Stores recent prices to detect anomalies: Pair -> Window of prices
    price_history: RwLock<HashMap<String, VecDeque<f64>>>,
    /// Max history length per pair
    window_size: usize,
}

impl Default for PreTradeValidator {
    fn default() -> Self {
        Self::new(20)
    }
}

impl PreTradeValidator {
    pub fn new(window_size: usize) -> Self {
        Self {
            price_history: RwLock::new(HashMap::new()),
            window_size,
        }
    }

    /// Validates the signal against hard sanity limits.
    pub fn validate_signal(&self, signal: &Signal) -> (bool, String) {
        if signal.cex_price <= 0.0 {
            return (false, "Invalid CEX price".to_string());
        }

        if signal.dex_price <= 0.0 {
            return (false, "Invalid DEX price".to_string());
        }

        if signal.spread_bps > 500.0 {
            return (
                false,
                format!(
                    "Spread {:.1}bps too high - likely bad data",
                    signal.spread_bps
                ),
            );
        }

        let age = signal.age_seconds();
        if age > 5.0 {
            return (false, format!("Signal too old: {:.1}s", age));
        }

        if signal.size <= 0.0 {
            return (false, "Invalid trade size".to_string());
        }

        (true, "OK".to_string())
    }

    /// Checks if a price deviates significantly from its recent average.
    /// Also updates the history with the new price.
    pub fn validate_price_feed(&self, price: f64, pair: &str) -> (bool, String) {
        if price <= 0.0 {
            return (false, "Price must be positive".to_string());
        }

        let mut history_map = self.price_history.write().unwrap();
        let history = history_map.entry(pair.to_string()).or_default();

        let recent_avg = if !history.is_empty() {
            history.iter().sum::<f64>() / history.len() as f64
        } else {
            0.0
        };

        if history.len() >= self.window_size {
            history.pop_front();
        }
        history.push_back(price);

        if recent_avg > 0.0 {
            let deviation = (price - recent_avg).abs() / recent_avg;
            if deviation > 0.05 {
                return (
                    false,
                    format!(
                        "Price {:.2} deviates {:.1}% from recent avg {:.2}",
                        price,
                        deviation * 100.0,
                        recent_avg
                    ),
                );
            }
        }

        (true, "OK".to_string())
    }
}
