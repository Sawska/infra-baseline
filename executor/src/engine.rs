use crate::flashbots::FlashbotsClient;
use crate::metrics::ExecutionMetrics;
use crate::recovery::{CircuitBreaker, CircuitBreakerConfig, ReplayProtection};
use alloy_primitives::{Address as AlloyAddress, Bytes, U256};
use alloy_sol_types::{SolCall, sol};
use arb_chain::builder::TransactionBuilder;
use arb_chain::client::ChainClient;
use arb_core::types::{TokenAmount, TransactionRequest};
use arb_core::{Address, WalletManager};
use exchange::client::ExchangeClient;
use inventory::tracker::{InventoryTracker, Venue};
use log::{debug, info, warn};
use pricing::amm::Pool;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use strategy::generator::SignalGenerator;
use strategy::signal::{Direction, Signal};
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep, timeout};

const MIN_NOTIONAL: f64 = 5.0;
const LOT_SIZE_STEP: f64 = 1.0;
const PRICE_TICK: f64 = 0.00000001;

pub fn round_quantity(qty: f64, step: f64) -> f64 {
    (qty / step).floor() * step
}

pub fn round_price(price: f64, tick: f64) -> f64 {
    (price / tick).round() * tick
}

sol! {
    function approve(address spender, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);

    function swapExactTokensForTokens(
        uint amountIn,
        uint amountOutMin,
        address[] calldata path,
        address to,
        uint deadline
    ) external returns (uint[] memory amounts);

    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

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
            started_at: SignalGenerator::get_now(),
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
    pub dex_router_v3: Option<String>,
    pub slippage_tolerance: f64,
    pub cb_config: Option<CircuitBreakerConfig>,
    pub dex_router_address: Option<String>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            leg1_timeout_sec: 15.0,
            leg2_timeout_sec: 60.0,
            min_fill_ratio: 0.99,
            use_flashbots: true,
            simulation_mode: true,
            dex_router_v3: None,
            slippage_tolerance: 0.05,
            dex_router_address: None,
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
    pub chain_client: Option<Arc<ChainClient>>,
    pub wallet: Option<Arc<WalletManager>>,
    token_addresses: HashMap<String, Address>,
    pools: HashMap<String, Pool>,
    inventory: Arc<Mutex<InventoryTracker>>,
    config: ExecutorConfig,
    circuit_breaker: Mutex<CircuitBreaker>,
    replay_protection: Mutex<ReplayProtection>,
    metrics: ExecutionMetrics,
    flashbots_client: Option<Arc<FlashbotsClient>>,
}

impl Executor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exchange: Arc<ExchangeClient>,
        chain_client: Option<Arc<ChainClient>>,
        wallet: Option<Arc<WalletManager>>,
        token_addresses: HashMap<String, Address>,
        pools: HashMap<String, Pool>,
        inventory: Arc<Mutex<InventoryTracker>>,
        config: Option<ExecutorConfig>,
        webhook_url: Option<String>,
    ) -> Self {
        let config = config.unwrap_or_default();

        let cb_config = config.cb_config.clone().unwrap_or(CircuitBreakerConfig {
            failure_threshold: 3,
            window_seconds: 300.0,
            cooldown_seconds: 600.0,
            webhook_url,
        });

        let flashbots_client = if config.use_flashbots {
            let relay_url = crate::config::APP_CONFIG.flashbots.relay_url.clone();
            let auth_key = crate::config::APP_CONFIG
                .flashbots
                .auth_key
                .clone_value()
                .unwrap_or_default();

            match FlashbotsClient::new(&relay_url, &auth_key) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    warn!(
                        "Failed to initialise Flashbots client: {}. \
                         DEX legs will fall back to the public mempool.",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        Self {
            exchange,
            chain_client,
            wallet,
            token_addresses,
            pools,
            inventory,
            config,
            circuit_breaker: Mutex::new(CircuitBreaker::new(Some(cb_config))),
            replay_protection: Mutex::new(ReplayProtection::new(60.0)),
            metrics: ExecutionMetrics::new(),
            flashbots_client,
        }
    }

    pub async fn execute(&self, signal: Signal) -> ExecutionContext {
        self.metrics.execution_attempts.inc();
        let mut ctx = ExecutionContext::new(signal.clone());

        {
            let mut cb = self.circuit_breaker.lock().await;
            if cb.is_open() {
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("Circuit breaker open".to_string());
                self.metrics.execution_failure.inc();
                return ctx;
            }
        }

        {
            let mut rp = self.replay_protection.lock().await;
            if rp.is_duplicate(&signal) {
                ctx.state = ExecutorState::Failed;
                ctx.error = Some("Duplicate signal".to_string());
                self.metrics.execution_failure.inc();
                return ctx;
            }
        }

        ctx.state = ExecutorState::Validating;
        if !signal.is_valid() {
            debug!("Invalid signal: {:?}", signal);
            ctx.state = ExecutorState::Failed;
            ctx.error = Some("Signal invalid".to_string());
            self.metrics.execution_failure.inc();
            return ctx;
        }

        let result_ctx = match signal.direction {
            Direction::BuyDexSellCex => self.execute_dex_first(ctx).await,
            Direction::BuyCexSellDex => self.execute_cex_first(ctx).await,
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
                warn!(
                    "Circuit breaker: {}/{} failures",
                    cb.failure_count(),
                    cb.failure_threshold()
                );
                if cb.is_open() {
                    warn!("Circuit breaker OPENED");
                }
            }
        }

        let mut final_ctx = result_ctx;
        final_ctx.finished_at = Some(SignalGenerator::get_now());

        if final_ctx.state == ExecutorState::Done {
            self.metrics.execution_success.inc();
            if let Some(pnl) = final_ctx.actual_net_pnl {
                self.metrics.net_pnl.observe(pnl);
            }
        } else {
            self.metrics.execution_failure.inc();
            warn!("Arb Failed: {:?} Pair={}", final_ctx.error, signal.pair);
        }

        final_ctx
    }

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
                self.metrics.leg_cex_failures.inc();
                return ctx;
            }
        };

        if !leg1.success {
            ctx.state = ExecutorState::Failed;
            ctx.error = leg1.error.or(Some("CEX rejected".to_string()));
            self.metrics.leg_cex_failures.inc();
            return ctx;
        }

        if leg1.filled / signal.size < self.config.min_fill_ratio {
            ctx.state = ExecutorState::Failed;
            ctx.error = Some(format!("Partial fill: {} < {}", leg1.filled, signal.size));
            self.metrics.leg_cex_failures.inc();
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
                self.metrics.leg_dex_failures.inc();
                return ctx;
            }
        };

        if !leg2.success {
            info!("DEX Failed, Unwinding. Signal: {:?}", signal);
            ctx.state = ExecutorState::Unwinding;
            self.unwind(&ctx).await;
            ctx.state = ExecutorState::Failed;
            ctx.error = Some(format!("DEX failed: {:?} - unwound", leg2.error));
            self.metrics.leg_dex_failures.inc();
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
                self.metrics.leg_dex_failures.inc();
                return ctx;
            }
        };

        if !leg1.success {
            ctx.state = ExecutorState::Failed;
            ctx.error = Some(format!("DEX failed: {:?}", leg1.error));
            self.metrics.leg_dex_failures.inc();
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
                self.metrics.leg_cex_failures.inc();
                return ctx;
            }
        };

        if !leg2.success {
            ctx.state = ExecutorState::Unwinding;
            self.unwind(&ctx).await;
            let err_msg = leg2.error.unwrap_or("Unknown CEX error".to_string());
            warn!("CEX error during leg 2: {}", err_msg);
            ctx.error = Some(format!("CEX failed: {} - unwound", err_msg));
            ctx.state = ExecutorState::Failed;
            self.metrics.leg_cex_failures.inc();
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
            sleep(Duration::from_millis(100)).await;

            if signal.pair.starts_with("CEXFAIL") {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("Simulated CEX failure".to_string()),
                };
            }

            if signal.pair.starts_with("PARTIAL") {
                return LegResult {
                    success: true,
                    price: signal.cex_price,
                    filled: actual_size * 0.5,
                    order_id: Some(format!("sim-partial-{}", SignalGenerator::get_now() as u64)),
                    tx_hash: None,
                    error: None,
                };
            }

            return LegResult {
                success: true,
                price: signal.cex_price,
                filled: actual_size,
                order_id: Some(format!("sim-cex-{}", SignalGenerator::get_now() as u64)),
                tx_hash: None,
                error: None,
            };
        }

        let side = if signal.direction == Direction::BuyCexSellDex {
            "buy"
        } else {
            "sell"
        };
        let price_multiplier = if side == "buy" { 1.01 } else { 0.99 };

        let qty = round_quantity(actual_size, LOT_SIZE_STEP);
        let price = round_price(signal.cex_price * price_multiplier, PRICE_TICK);

        if qty * price < MIN_NOTIONAL {
            return LegResult {
                success: false,
                price,
                filled: 0.0,
                order_id: None,
                tx_hash: None,
                error: Some("Order below MIN_NOTIONAL".to_string()),
            };
        }

        let limit_price = Decimal::from_f64(price).unwrap_or(Decimal::ZERO);
        let amount = TokenAmount::from_human(&qty.to_string(), 8, None).unwrap();

        match self
            .exchange
            .create_limit_ioc_order(&signal.pair, side, amount, limit_price)
            .await
        {
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
            sleep(Duration::from_millis(100)).await;

            if signal.pair.starts_with("DEXFAIL") {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("Simulated DEX failure".to_string()),
                };
            }

            if signal.pair.starts_with("PARTIAL") {
                return LegResult {
                    success: true,
                    price: signal.dex_price,
                    filled: size * 0.5,
                    order_id: None,
                    tx_hash: Some("0xsimulatedpartial".to_string()),
                    error: None,
                };
            }

            return LegResult {
                success: true,
                price: signal.dex_price,
                filled: size,
                order_id: None,
                tx_hash: Some("0xsimulatedhash".to_string()),
                error: None,
            };
        }

        let client = match &self.chain_client {
            Some(c) => c,
            None => {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("ChainClient missing".to_string()),
                };
            }
        };

        let wallet = match &self.wallet {
            Some(w) => w,
            None => {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("Wallet missing".to_string()),
                };
            }
        };

        let parts: Vec<&str> = signal.pair.split('/').collect();
        if parts.len() != 2 {
            return LegResult {
                success: false,
                price: 0.0,
                filled: 0.0,
                order_id: None,
                tx_hash: None,
                error: Some("Invalid pair fmt".to_string()),
            };
        }
        let (base_sym, quote_sym) = (parts[0], parts[1]);

        let (token_in_sym, token_out_sym) = match signal.direction {
            Direction::BuyCexSellDex => (base_sym, quote_sym),
            Direction::BuyDexSellCex => (quote_sym, base_sym),
        };

        let t_in = match self.token_addresses.get(token_in_sym) {
            Some(a) => a,
            None => {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some(format!("Token not found: {}", token_in_sym)),
                };
            }
        };
        let t_out = match self.token_addresses.get(token_out_sym) {
            Some(a) => a,
            None => {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some(format!("Token not found: {}", token_out_sym)),
                };
            }
        };

        let token_in_addr = AlloyAddress::from(*t_in.0);
        let token_out_addr = AlloyAddress::from(*t_out.0);

        let pool = self.pools.get(&signal.pair);
        let is_v3 = matches!(pool, Some(Pool::V3(_)));

        let router_addr_str = if is_v3 {
            self.config.dex_router_v3.as_deref().unwrap_or("")
        } else {
            self.config.dex_router_address.as_deref().unwrap_or("")
        };

        if router_addr_str.is_empty() {
            return LegResult {
                success: false,
                price: 0.0,
                filled: 0.0,
                order_id: None,
                tx_hash: None,
                error: Some("Router address missing".to_string()),
            };
        }

        let router_addr = match Address::from_string(router_addr_str) {
            Ok(a) => a,
            Err(_) => {
                return LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some("Invalid router address".to_string()),
                };
            }
        };

        let decimals = if let Some(p) = pool {
            let (t0, t1) = p.tokens();
            if t0.address == *t_in {
                t0.decimals
            } else {
                t1.decimals
            }
        } else {
            18
        };

        let (amount_in_u256, expected_fill_size) = if signal.direction == Direction::BuyDexSellCex {
            let usdt_cost = size * signal.dex_price;
            let input_val = U256::from((usdt_cost * 10f64.powi(decimals as i32)).round() as u128);
            (input_val, size)
        } else {
            let input_val = U256::from((size * 10f64.powi(decimals as i32)).round() as u128);
            (input_val, size)
        };

        let amount_out_min = if let Some(p) = pool {
            let (t0, t1) = p.tokens();
            let token_in_ref = if t0.address == *t_in { &t0 } else { &t1 };

            match p.get_amount_out(amount_in_u256, token_in_ref) {
                Ok(expected) => {
                    let mut tolerance_bps = (self.config.slippage_tolerance * 10000.0) as u64;
                    if matches!(p, Pool::V3(_)) {
                        tolerance_bps += 50;
                    }
                    let factor = U256::from(10000_u64.saturating_sub(tolerance_bps));
                    expected
                        .checked_mul(factor)
                        .unwrap_or(U256::ZERO)
                        .checked_div(U256::from(10000))
                        .unwrap_or(U256::ZERO)
                }
                Err(e) => {
                    warn!(
                        "Failed to calculate output for slippage: {}. Using unsafe fallback.",
                        e
                    );
                    U256::from(1)
                }
            }
        } else {
            warn!("Pool not found for calc. Using unsafe slippage.");
            U256::from(1)
        };

        if let Err(e) = self
            .ensure_allowance(client, wallet, *t_in, router_addr, amount_in_u256)
            .await
        {
            return LegResult {
                success: false,
                price: 0.0,
                filled: 0.0,
                order_id: None,
                tx_hash: None,
                error: Some(format!("Allowance failed: {}", e)),
            };
        }

        let deadline = U256::from(SignalGenerator::get_now() as u64 + 300);
        let calldata = if is_v3 {
            let pool_v3 = match pool {
                Some(Pool::V3(p)) => p,
                _ => {
                    return LegResult {
                        success: false,
                        price: 0.0,
                        filled: 0.0,
                        order_id: None,
                        tx_hash: None,
                        error: Some("Pool mismatch".into()),
                    };
                }
            };

            let params = ExactInputSingleParams {
                tokenIn: token_in_addr,
                tokenOut: token_out_addr,
                fee: alloy_primitives::Uint::from(pool_v3.fee),
                recipient: AlloyAddress::from(*wallet.address().0),
                deadline,
                amountIn: amount_in_u256,
                amountOutMinimum: amount_out_min,
                sqrtPriceLimitX96: alloy_primitives::U160::ZERO,
            };
            exactInputSingleCall { params }.abi_encode()
        } else {
            swapExactTokensForTokensCall {
                amountIn: amount_in_u256,
                amountOutMin: amount_out_min,
                path: vec![token_in_addr, token_out_addr],
                to: AlloyAddress::from(*wallet.address().0),
                deadline,
            }
            .abi_encode()
        };

        let builder = TransactionBuilder::new(client, wallet)
            .to(router_addr)
            .data(calldata)
            .gas_limit(500_000)
            .with_gas_price("high");

        if let Some(fb) = &self.flashbots_client {
            let raw_signed_tx = match builder.build_and_sign().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    return LegResult {
                        success: false,
                        price: 0.0,
                        filled: 0.0,
                        order_id: None,
                        tx_hash: None,
                        error: Some(format!("Failed to sign tx for bundle: {:?}", e)),
                    };
                }
            };

            let current_block = client.get_block_number().await.unwrap_or(0);
            let target_block = current_block + 1;

            match fb
                .send_bundle(vec![Bytes::from(raw_signed_tx)], target_block)
                .await
            {
                Ok(bundle_hash) => {
                    let included = client.wait_for_tx(bundle_hash).await.unwrap_or(false);

                    if included {
                        LegResult {
                            success: true,
                            price: signal.dex_price,
                            filled: expected_fill_size,
                            order_id: None,
                            tx_hash: Some(bundle_hash.to_string()),
                            error: None,
                        }
                    } else {
                        LegResult {
                            success: false,
                            price: 0.0,
                            filled: 0.0,
                            order_id: None,
                            tx_hash: None,
                            error: Some(format!("Bundle missed target block {}", target_block)),
                        }
                    }
                }
                Err(e) => LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some(format!("Bundle submission failed: {:?}", e)),
                },
            }
        } else {
            match builder.send_and_wait().await {
                Ok(receipt) => {
                    if receipt.status {
                        LegResult {
                            success: true,
                            price: signal.dex_price,
                            filled: expected_fill_size,
                            order_id: None,
                            tx_hash: Some(receipt.tx_hash),
                            error: None,
                        }
                    } else {
                        LegResult {
                            success: false,
                            price: 0.0,
                            filled: 0.0,
                            order_id: None,
                            tx_hash: Some(receipt.tx_hash),
                            error: Some("Tx Reverted".to_string()),
                        }
                    }
                }
                Err(e) => LegResult {
                    success: false,
                    price: 0.0,
                    filled: 0.0,
                    order_id: None,
                    tx_hash: None,
                    error: Some(format!("Tx Failed: {:?}", e)),
                },
            }
        }
    }

    async fn ensure_allowance(
        &self,
        client: &ChainClient,
        wallet: &WalletManager,
        token: Address,
        spender: Address,
        amount: U256,
    ) -> Result<(), String> {
        let owner_alloy = AlloyAddress::from(*wallet.address().0);
        let spender_alloy = AlloyAddress::from(*spender.0);

        let call = allowanceCall {
            owner: owner_alloy,
            spender: spender_alloy,
        };

        let req = TransactionRequest {
            to: token,
            from: Some(wallet.address()),
            value: TokenAmount {
                raw: U256::ZERO,
                decimals: 18,
                symbol: None,
            },
            data: Bytes::from(call.abi_encode()),
            chain_id: 4216,
            nonce: None,
            gas_limit: None,
            max_fee_per_gas: None,
            max_priority_fee: None,
        };

        let res = client
            .call(&req, None)
            .await
            .map_err(|e| format!("{:?}", e))?;
        let current_allowance =
            allowanceCall::abi_decode_returns(&res).map_err(|_| "Decode fail on allowance")?;

        if current_allowance < amount {
            if current_allowance > U256::ZERO {
                let reset_data = approveCall {
                    spender: spender_alloy,
                    amount: U256::ZERO,
                }
                .abi_encode();

                let _ = TransactionBuilder::new(client, wallet)
                    .to(token)
                    .data(reset_data)
                    .gas_limit(100_000)
                    .send_and_wait()
                    .await;
            }

            let approve_data = approveCall {
                spender: spender_alloy,
                amount: U256::MAX,
            }
            .abi_encode();

            let receipt = TransactionBuilder::new(client, wallet)
                .to(token)
                .data(approve_data)
                .gas_limit(100_000)
                .send_and_wait()
                .await
                .map_err(|e| format!("Approve failed: {:?}", e))?;

            if !receipt.status {
                return Err("Approve reverted on-chain".to_string());
            }
        }

        Ok(())
    }

    async fn unwind(&self, _ctx: &ExecutionContext) {
        if self.config.simulation_mode {
            sleep(Duration::from_millis(100)).await;
            return;
        }
        warn!(
            "Unwind triggered! Manual intervention needed for pair {}",
            _ctx.signal.pair
        );
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

        gross - (size * leg1_price * 0.002)
    }

    pub async fn is_circuit_open(&self) -> bool {
        self.circuit_breaker.lock().await.is_open()
    }
}
