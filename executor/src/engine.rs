use crate::recovery::{CircuitBreaker, CircuitBreakerConfig, ReplayProtection};
use arb_core::types::TokenAmount;
use exchange::client::ExchangeClient;
use inventory::tracker::{InventoryTracker, Venue};
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use strategy::signal::{Direction, Signal};
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep, timeout};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutorState {
    Idle,
    Validating,
    Leg1Pending,
    Leg1Filled,
    Leg2Pending,
    Done,
    Failed,
    Unwinding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionContext {
    pub signal: Signal,
    pub state: ExecutorState,

    pub leg1_venue: String,
    pub leg1_order_id: Option<String>,
    pub leg1_fill_price: Option<f64>,
    pub leg1_fill_size: Option<f64>,

    pub leg2_venue: String,
    pub leg2_tx_hash: Option<String>,
    pub leg2_fill_price: Option<f64>,
    pub leg2_fill_size: Option<f64>,

    pub started_at: f64,
    pub finished_at: Option<f64>,
    pub actual_net_pnl: Option<f64>,
    pub error: Option<String>,
}

impl ExecutionContext {
    pub fn new(signal: Signal) -> Self {
        Self {
            signal,
            state: ExecutorState::Idle,
            leg1_venue: String::new(),
            leg1_order_id: None,
            leg1_fill_price: None,
            leg1_fill_size: None,
            leg2_venue: String::new(),
            leg2_tx_hash: None,
            leg2_fill_price: None,
            leg2_fill_size: None,
            started_at: get_now(),
            finished_at: None,
            actual_net_pnl: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    pub leg1_timeout_sec: f64,
    pub leg2_timeout_sec: f64,
    pub min_fill_ratio: f64,
    pub use_flashbots: bool,
    pub simulation_mode: bool,
    // Configuration for the internal Circuit Breaker
    pub cb_config: Option<CircuitBreakerConfig>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            leg1_timeout_sec: 5.0,
            leg2_timeout_sec: 60.0,
            min_fill_ratio: 0.8,
            use_flashbots: true,
            simulation_mode: true,
            cb_config: None,
        }
    }
}

struct LegResult {
    success: bool,
    price: f64,
    filled: f64,
    order_id: Option<String>,
    tx_hash: Option<String>,
    error: Option<String>,
}

pub struct Executor {
    exchange: Arc<ExchangeClient>,
    inventory: Arc<Mutex<InventoryTracker>>,
    config: ExecutorConfig,
    circuit_breaker: Mutex<CircuitBreaker>,
    replay_protection: Mutex<ReplayProtection>,
}

impl Executor {
    pub fn new(
        exchange: Arc<ExchangeClient>,
        inventory: Arc<Mutex<InventoryTracker>>,
        config: Option<ExecutorConfig>,
    ) -> Self {
        let config = config.unwrap_or_default();

        let cb_config = config.cb_config.clone().unwrap_or(CircuitBreakerConfig {
            failure_threshold: 3,
            window_seconds: 300.0,
            cooldown_seconds: 600.0,
        });

        Self {
            exchange,
            inventory,
            config,
            circuit_breaker: Mutex::new(CircuitBreaker::new(Some(cb_config))),
            replay_protection: Mutex::new(ReplayProtection::new(60.0)),
        }
    }

    pub async fn execute(&self, signal: Signal) -> ExecutionContext {
        let mut ctx = ExecutionContext::new(signal.clone());

        {
            let mut cb = self.circuit_breaker.lock().await;
            if cb.is_open() {
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("Circuit breaker open".to_string());
                return ctx;
            }
        }

        {
            let mut rp = self.replay_protection.lock().await;
            if rp.is_duplicate(&signal) {
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("Duplicate signal".to_string());
                return ctx;
            }
        }

        ctx.state = ExecutorState::Validating;
        if !signal.is_valid() {
            ctx.state = ExecutorState::Failed;
            ctx.error = Some("Signal invalid".to_string());
            return ctx;
        }

        let result_ctx = if self.config.use_flashbots {
            self.execute_dex_first(ctx).await
        } else {
            self.execute_cex_first(ctx).await
        };

        {
            let mut rp = self.replay_protection.lock().await;
            rp.mark_executed(&signal);
        }

        {
            let mut cb = self.circuit_breaker.lock().await;
            if result_ctx.state == ExecutorState::Done {
                cb.record_success();
            } else {
                cb.record_failure();
            }
        }

        let mut final_ctx = result_ctx;
        final_ctx.finished_at = Some(get_now());
        final_ctx
    }

    /// Helper to update inventory tracker after a trade execution
    async fn update_inventory(
        &self,
        venue_str: &str,
        signal: &Signal,
        filled_size: f64,
        fill_price: f64,
    ) {
        let mut tracker = self.inventory.lock().await;
        let parts: Vec<&str> = signal.pair.split('/').collect();
        if parts.len() != 2 {
            return;
        }
        let base = parts[0];
        let quote = parts[1];

        let venue = if venue_str == "cex" {
            Venue::Cex
        } else {
            Venue::Wallet
        };

        let side = match (signal.direction, venue) {
            (Direction::BuyCexSellDex, Venue::Cex) => "buy",
            (Direction::BuyCexSellDex, Venue::Wallet) => "sell",
            (Direction::BuyDexSellCex, Venue::Cex) => "sell",
            (Direction::BuyDexSellCex, Venue::Wallet) => "buy",
        };

        let base_amount = Decimal::from_f64(filled_size).unwrap_or(Decimal::ZERO);
        let quote_amount = Decimal::from_f64(filled_size * fill_price).unwrap_or(Decimal::ZERO);

        let fee = if side == "buy" {
            base_amount * Decimal::from_str("0.001").unwrap_or(Decimal::ZERO)
        } else {
            quote_amount * Decimal::from_str("0.001").unwrap_or(Decimal::ZERO)
        };
        let fee_asset = if side == "buy" { base } else { quote };

        tracker.record_trade(
            venue,
            side,
            base,
            quote,
            base_amount,
            quote_amount,
            fee,
            fee_asset,
        );
    }

    async fn execute_cex_first(&self, mut ctx: ExecutionContext) -> ExecutionContext {
        let signal = ctx.signal.clone();

        ctx.state = ExecutorState::Leg1Pending;
        ctx.leg1_venue = "cex".to_string();

        let leg1 = match timeout(
            Duration::from_secs_f64(self.config.leg1_timeout_sec),
            self.execute_cex_leg(&signal, None),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => {
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("CEX timeout".to_string());
                return ctx;
            }
        };

        if !leg1.success {
            ctx.state = ExecutorState::Failed;
            ctx.error = leg1.error.or(Some("CEX rejected".to_string()));
            return ctx;
        }

        if leg1.filled / signal.size < self.config.min_fill_ratio {
            ctx.state = ExecutorState::Failed;
            ctx.error = Some("Partial fill below threshold".to_string());
            return ctx;
        }

        self.update_inventory("cex", &signal, leg1.filled, leg1.price)
            .await;

        ctx.leg1_fill_price = Some(leg1.price);
        ctx.leg1_fill_size = Some(leg1.filled);
        ctx.leg1_order_id = leg1.order_id;
        ctx.state = ExecutorState::Leg1Filled;

        ctx.state = ExecutorState::Leg2Pending;
        ctx.leg2_venue = "dex".to_string();

        let leg2 = match timeout(
            Duration::from_secs_f64(self.config.leg2_timeout_sec),
            self.execute_dex_leg(&signal, ctx.leg1_fill_size.unwrap()),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => {
                ctx.state = ExecutorState::Unwinding;
                self.unwind(&ctx).await;
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("DEX timeout - unwound".to_string());
                return ctx;
            }
        };

        if !leg2.success {
            ctx.state = ExecutorState::Unwinding;
            self.unwind(&ctx).await;
            ctx.state = ExecutorState::Failed;
            ctx.error = Some("DEX failed - unwound".to_string());
            return ctx;
        }

        self.update_inventory("dex", &signal, leg2.filled, leg2.price)
            .await;

        ctx.leg2_fill_price = Some(leg2.price);
        ctx.leg2_fill_size = Some(leg2.filled);
        ctx.leg2_tx_hash = leg2.tx_hash;
        ctx.actual_net_pnl = Some(self.calculate_pnl(&ctx));
        ctx.state = ExecutorState::Done;
        ctx
    }

    async fn execute_dex_first(&self, mut ctx: ExecutionContext) -> ExecutionContext {
        let signal = ctx.signal.clone();

        // Leg 1: DEX
        ctx.state = ExecutorState::Leg1Pending;
        ctx.leg1_venue = "dex".to_string();

        let leg1 = match timeout(
            Duration::from_secs_f64(self.config.leg2_timeout_sec),
            self.execute_dex_leg(&signal, signal.size),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => {
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("DEX timeout".to_string());
                return ctx;
            }
        };

        if !leg1.success {
            ctx.state = ExecutorState::Failed;
            ctx.error = Some("DEX failed (no cost via Flashbots)".to_string());
            return ctx;
        }

        self.update_inventory("dex", &signal, leg1.filled, leg1.price)
            .await;

        ctx.leg1_fill_price = Some(leg1.price);
        ctx.leg1_fill_size = Some(leg1.filled);
        ctx.leg1_order_id = leg1.tx_hash;
        ctx.state = ExecutorState::Leg1Filled;

        ctx.state = ExecutorState::Leg2Pending;
        ctx.leg2_venue = "cex".to_string();

        let leg2 = match timeout(
            Duration::from_secs_f64(self.config.leg1_timeout_sec),
            self.execute_cex_leg(&signal, ctx.leg1_fill_size),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => {
                ctx.state = ExecutorState::Unwinding;
                self.unwind(&ctx).await;
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("CEX timeout after DEX - unwound".to_string());
                return ctx;
            }
        };

        if !leg2.success {
            ctx.state = ExecutorState::Unwinding;
            self.unwind(&ctx).await;
            ctx.state = ExecutorState::Failed;
            ctx.error = Some("CEX failed after DEX - unwound".to_string());
            return ctx;
        }

        self.update_inventory("cex", &signal, leg2.filled, leg2.price)
            .await;

        ctx.leg2_fill_price = Some(leg2.price);
        ctx.leg2_fill_size = Some(leg2.filled);
        ctx.leg2_tx_hash = leg2.order_id;
        ctx.actual_net_pnl = Some(self.calculate_pnl(&ctx));
        ctx.state = ExecutorState::Done;
        ctx
    }

    async fn execute_cex_leg(&self, signal: &Signal, size: Option<f64>) -> LegResult {
        let actual_size = size.unwrap_or(signal.size);

        if self.config.simulation_mode {
            if signal.pair.contains("CEXFAIL") {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("Simulated CEX Failure".to_string()),
                };
            }
            if signal.pair.contains("PARTIAL") {
                sleep(Duration::from_millis(100)).await;
                return LegResult {
                    success: true,
                    price: signal.cex_price * 1.0001,
                    filled: actual_size * 0.5, // Return 50% fill
                    order_id: Some("sim-cex-partial".to_string()),
                    tx_hash: None,
                    error: None,
                };
            }

            sleep(Duration::from_millis(100)).await;
            return LegResult {
                success: true,
                price: signal.cex_price * 1.0001,
                filled: actual_size,
                order_id: Some("sim-cex-order".to_string()),
                tx_hash: None,
                error: None,
            };
        }

        let side = if signal.direction == Direction::BuyCexSellDex {
            "buy"
        } else {
            "sell"
        };
        let price_multiplier = if side == "buy" { 1.001 } else { 0.999 };
        let limit_price =
            Decimal::from_f64(signal.cex_price * price_multiplier).unwrap_or(Decimal::ZERO);
        let amount = TokenAmount::from_human(&actual_size.to_string(), 8, None).unwrap();

        let result = self
            .exchange
            .create_limit_ioc_order(&signal.pair, side, amount, limit_price)
            .await;

        match result {
            Ok(order) => LegResult {
                success: order.status == "filled",
                price: order.avg_fill_price.to_f64().unwrap_or(0.0),
                filled: order.amount_filled.to_human().to_f64().unwrap_or(0.0),
                order_id: Some(order.id),
                tx_hash: None,
                error: if order.status == "filled" {
                    None
                } else {
                    Some(order.status)
                },
            },
            Err(e) => LegResult {
                success: false,
                price: 0.0,
                filled: 0.0,
                order_id: None,
                tx_hash: None,
                error: Some(format!("{:?}", e)),
            },
        }
    }

    async fn execute_dex_leg(&self, signal: &Signal, size: f64) -> LegResult {
        if self.config.simulation_mode {
            if signal.pair.contains("DEXFAIL") {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("Simulated DEX Failure".to_string()),
                };
            }

            sleep(Duration::from_millis(500)).await;
            return LegResult {
                success: true,
                price: signal.dex_price * 0.9998,
                filled: size,
                order_id: None,
                tx_hash: Some("0xsimulatedhash".to_string()),
                error: None,
            };
        }

        LegResult {
            success: false,
            price: 0.0,
            filled: 0.0,
            order_id: None,
            tx_hash: None,
            error: Some("Real DEX execution requires Week 2 integration".to_string()),
        }
    }

    async fn unwind(&self, _ctx: &ExecutionContext) {
        if self.config.simulation_mode {
            sleep(Duration::from_millis(100)).await;
            return;
        }
        log::error!("Real unwind not implemented");
    }

    fn calculate_pnl(&self, ctx: &ExecutionContext) -> f64 {
        let leg1_price = ctx.leg1_fill_price.unwrap_or(0.0);
        let leg2_price = ctx.leg2_fill_price.unwrap_or(0.0);
        let size = ctx.leg1_fill_size.unwrap_or(0.0);

        let gross = if ctx.signal.direction == Direction::BuyCexSellDex {
            if ctx.leg1_venue == "cex" {
                (leg2_price - leg1_price) * size
            } else {
                (leg1_price - leg2_price) * size
            }
        } else if ctx.leg1_venue == "dex" {
            (leg2_price - leg1_price) * size
        } else {
            (leg1_price - leg2_price) * size
        };

        let fees = size * leg1_price * 0.004;
        gross - fees
    }
}

fn get_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
