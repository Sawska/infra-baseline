use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::thread;
use strategy::generator::SignalGenerator;
use strategy::signal::Signal;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: usize,
    pub window_seconds: f64,
    pub cooldown_seconds: f64,
    pub webhook_url: Option<String>,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            window_seconds: 300.0,
            cooldown_seconds: 600.0,
            webhook_url: None,
        }
    }
}

pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    failures: VecDeque<f64>,
    tripped_at: Option<f64>,
}

impl CircuitBreaker {
    pub fn new(config: Option<CircuitBreakerConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
            failures: VecDeque::new(),
            tripped_at: None,
        }
    }

    pub fn record_failure(&mut self) {
        let now = SignalGenerator::get_now();
        self.failures.push_back(now);
        self.cleanup_old_failures(now);

        if self.failures.len() >= self.config.failure_threshold {
            self.trip(now);
        }
    }

    pub fn record_success(&mut self) {}

    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }

    pub fn failure_threshold(&self) -> usize {
        self.config.failure_threshold
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped_at.is_some()
    }

    fn trip(&mut self, now: f64) {
        if self.tripped_at.is_none() {
            self.tripped_at = Some(now);

            if let Some(url) = &self.config.webhook_url {
                self.send_alert(url.clone(), now, self.failures.len());
            }
        }
    }

    /// Spawns a thread to send the webhook so the main trading loop isn't blocked.
    fn send_alert(&self, url: String, timestamp: f64, failure_count: usize) {
        let payload = json!({
            "alert_type": "CIRCUIT_BREAKER_TRIPPED",
            "timestamp": timestamp,
            "failures_detected": failure_count,
            "cooldown_seconds": self.config.cooldown_seconds,
            "message": format!("Circuit Breaker tripped after {} failures. Trading paused.", failure_count)
        });

        thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            match client.post(&url).json(&payload).send() {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        eprintln!("Failed to send webhook: Status {}", resp.status());
                    }
                }
                Err(e) => eprintln!("Failed to send webhook: {}", e),
            }
        });
    }

    pub fn is_open(&mut self) -> bool {
        if let Some(tripped_at) = self.tripped_at {
            let now = SignalGenerator::get_now();
            if now - tripped_at > self.config.cooldown_seconds {
                self.reset();
                return false;
            }
            return true;
        }
        false
    }

    fn reset(&mut self) {
        self.tripped_at = None;
        self.failures.clear();
    }

    fn cleanup_old_failures(&mut self, now: f64) {
        let cutoff = now - self.config.window_seconds;
        while let Some(&t) = self.failures.front() {
            if t <= cutoff {
                self.failures.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn time_until_reset(&self) -> f64 {
        match self.tripped_at {
            Some(t) => (self.config.cooldown_seconds - (SignalGenerator::get_now() - t)).max(0.0),
            None => 0.0,
        }
    }
}

pub struct ReplayProtection {
    executed: HashMap<String, f64>,
    ttl_seconds: f64,
}

impl ReplayProtection {
    pub fn new(ttl_seconds: f64) -> Self {
        Self {
            executed: HashMap::new(),
            ttl_seconds,
        }
    }

    pub fn is_duplicate(&mut self, signal: &Signal) -> bool {
        self.cleanup();
        self.executed.contains_key(&signal.signal_id)
    }

    pub fn mark_executed(&mut self, signal: &Signal) {
        self.executed
            .insert(signal.signal_id.clone(), SignalGenerator::get_now());
    }

    fn cleanup(&mut self) {
        let now = SignalGenerator::get_now();
        let cutoff = now - self.ttl_seconds;
        self.executed.retain(|_, &mut time| time > cutoff);
    }
}
