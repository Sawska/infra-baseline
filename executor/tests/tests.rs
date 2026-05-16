use arb_core::Address;
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use executor::engine::{Executor, ExecutorConfig, ExecutorState};
use executor::gas_oracle::{GasOracle, safety_check_with_gas};
use executor::kill_switch::AutoKillSwitch;
use executor::monitoring;
use executor::position_limits::{RiskLimits, RiskManager};
use executor::recovery::CircuitBreakerConfig;
use executor::telegram_alert::TelegramAlert;
use executor::validator::PreTradeValidator;
use inventory::tracker::InventoryTracker;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use strategy::signal::{Direction, Signal};
use tokio::sync::Mutex;
use tokio::time::sleep;

async fn setup_executor(config: ExecutorConfig) -> Executor {
    let ex_config = ExchangeConfig {
        api_key: "test".to_string(),
        secret: "test".to_string(),
        is_sandbox: true,
        skip_connection_validation: true,
        production: false,
        binance_http_url: "test".into(),
        binance_ws_url: "test".into(),
        cex_fee_bps: 0.0,
        arbitrum_rpc_url: "test".into(),
        arbitrum_chain_id: 0,
        pair: "test".into(),
        weth_address: "test".into(),
        usdc_address: "test".into(),
    };
    let exchange = Arc::new(
        ExchangeClient::new(ex_config, ExchangeType::Binance)
            .await
            .unwrap(),
    );
    let inventory = Arc::new(Mutex::new(InventoryTracker::new(None)));

    let token_addresses: HashMap<String, Address> = HashMap::new();

    Executor::new(
        exchange,
        None,
        None,
        token_addresses,
        HashMap::new(),
        inventory,
        Some(config),
        None,
    )
}

fn create_test_signal(pair: &str) -> Signal {
    Signal::new(
        pair.to_string(),
        Direction::BuyCexSellDex,
        2000.0,
        2020.0,
        100.0,
        1.0,
        20.0,
        5.0,
        15.0,
        80.0,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            + 60.0,
        true,
        true,
    )
}

#[tokio::test]
async fn test_execute_success() {
    let config = ExecutorConfig {
        use_flashbots: false,
        simulation_mode: true,
        ..Default::default()
    };
    let executor = setup_executor(config).await;
    let signal = create_test_signal("ETH/USDC");

    let ctx = executor.execute(signal).await;

    assert_eq!(ctx.state, ExecutorState::Done);
    assert!(ctx.leg1_fill_size.is_some());
    assert!(ctx.leg2_fill_size.is_some());
    assert!(ctx.finished_at.is_some());
    assert!(ctx.error.is_none());
}

#[tokio::test]
async fn test_execute_cex_timeout() {
    let config = ExecutorConfig {
        use_flashbots: false,
        simulation_mode: true,
        leg1_timeout_sec: 0.01,
        ..Default::default()
    };
    let executor = setup_executor(config).await;
    let signal = create_test_signal("ETH/USDC");

    let ctx = executor.execute(signal).await;

    assert_eq!(ctx.state, ExecutorState::Failed);
    assert!(ctx.error.as_ref().unwrap().contains("timeout"));
}

#[tokio::test]
async fn test_execute_dex_failure_unwinds() {
    let config = ExecutorConfig {
        use_flashbots: false,
        simulation_mode: true,
        ..Default::default()
    };
    let executor = setup_executor(config).await;
    let signal = create_test_signal("DEXFAIL/USDC");

    let ctx = executor.execute(signal).await;

    assert_eq!(ctx.state, ExecutorState::Failed);
    assert_eq!(ctx.leg1_venue, "cex");
    assert!(ctx.leg1_fill_size.is_some());
    assert!(ctx.error.as_ref().unwrap().contains("unwound"));
}

#[tokio::test]
async fn test_partial_fill_rejected() {
    let config = ExecutorConfig {
        use_flashbots: false,
        simulation_mode: true,
        min_fill_ratio: 0.8,
        ..Default::default()
    };
    let executor = setup_executor(config).await;
    let signal = create_test_signal("PARTIAL/USDC");

    let ctx = executor.execute(signal).await;

    assert_eq!(ctx.state, ExecutorState::Failed);
    assert!(ctx.error.as_ref().unwrap().contains("Partial fill"));
}

#[tokio::test]
async fn test_circuit_breaker_blocks() {
    let config = ExecutorConfig {
        use_flashbots: false,
        simulation_mode: true,
        leg1_timeout_sec: 0.01,
        ..Default::default()
    };
    let executor = setup_executor(config).await;

    for _ in 0..3 {
        let signal = create_test_signal("ETH/USDC");
        let _ = executor.execute(signal).await;
    }

    let signal = create_test_signal("ETH/USDC");
    let ctx = executor.execute(signal).await;

    assert_eq!(ctx.state, ExecutorState::Failed);
    assert_eq!(ctx.error, Some("Circuit breaker open".to_string()));
}

#[tokio::test]
async fn test_circuit_breaker_trips() {
    let cb_config = CircuitBreakerConfig {
        failure_threshold: 3,
        window_seconds: 60.0,
        cooldown_seconds: 60.0,
        webhook_url: None,
    };
    let config = ExecutorConfig {
        simulation_mode: true,
        cb_config: Some(cb_config),
        ..Default::default()
    };
    let executor = setup_executor(config).await;

    for _ in 0..3 {
        let signal = create_test_signal("CEXFAIL/USDC");
        let _ = executor.execute(signal).await;
    }

    let signal = create_test_signal("ETH/USDC");
    let ctx = executor.execute(signal).await;

    assert_eq!(ctx.state, ExecutorState::Failed);
    assert_eq!(ctx.error, Some("Circuit breaker open".to_string()));
}

#[tokio::test]
async fn test_replay_protection() {
    let config = ExecutorConfig::default();
    let executor = setup_executor(config).await;
    let signal = create_test_signal("ETH/USDC");

    let ctx1 = executor.execute(signal.clone()).await;
    assert_eq!(ctx1.state, ExecutorState::Done);

    let ctx2 = executor.execute(signal).await;

    assert_eq!(ctx2.state, ExecutorState::Failed);
    assert_eq!(ctx2.error, Some("Duplicate signal".to_string()));
}

#[tokio::test]
async fn test_circuit_breaker_resets() {
    let cb_config = CircuitBreakerConfig {
        failure_threshold: 1,
        window_seconds: 60.0,
        cooldown_seconds: 0.1,
        webhook_url: None,
    };
    let config = ExecutorConfig {
        simulation_mode: true,
        cb_config: Some(cb_config),
        ..Default::default()
    };
    let executor = setup_executor(config).await;

    let signal1 = create_test_signal("CEXFAIL/USDC");
    let _ = executor.execute(signal1).await;

    let signal2 = create_test_signal("ETH/USDC");
    let ctx_blocked = executor.execute(signal2).await;
    assert_eq!(ctx_blocked.state, ExecutorState::Failed);
    assert!(ctx_blocked.error.unwrap().contains("Circuit breaker open"));

    sleep(Duration::from_millis(200)).await;

    let signal3 = create_test_signal("ETH/USDC");
    let ctx_success = executor.execute(signal3).await;
    assert_eq!(ctx_success.state, ExecutorState::Done);
}

#[tokio::test]
async fn test_replay_blocks_duplicate() {
    let config = ExecutorConfig::default();
    let executor = setup_executor(config).await;
    let signal = create_test_signal("ETH/USDC");

    let ctx1 = executor.execute(signal.clone()).await;
    assert_eq!(ctx1.state, ExecutorState::Done);

    let ctx2 = executor.execute(signal).await;

    assert_eq!(ctx2.state, ExecutorState::Failed);
    assert_eq!(ctx2.error, Some("Duplicate signal".to_string()));
}

#[tokio::test]
async fn test_replay_allows_new() {
    let config = ExecutorConfig::default();
    let executor = setup_executor(config).await;

    let signal1 = create_test_signal("ETH/USDC");
    let ctx1 = executor.execute(signal1).await;
    assert_eq!(ctx1.state, ExecutorState::Done);

    let signal2 = create_test_signal("ETH/USDC");
    let ctx2 = executor.execute(signal2).await;

    assert_eq!(ctx2.state, ExecutorState::Done);
}

#[test]
fn test_risk_manager_trade_size_limits() {
    let limits = RiskLimits {
        max_trade_usd: 100.0,
        max_trade_pct: 0.10,
        ..Default::default()
    };
    let manager = RiskManager::new(limits, 1000.0);

    let mut signal = create_test_signal("ETH/USDC");
    signal.size = 1.0;
    signal.cex_price = 150.0;
    let (allowed, reason) = manager.check_pre_trade(&signal);
    assert!(!allowed, "Should be rejected due to fixed USD limit");
    assert!(reason.contains("exceeds max $100.00"));

    let limits_pct = RiskLimits {
        max_trade_usd: 5000.0,
        max_trade_pct: 0.10,
        ..Default::default()
    };
    let manager_pct = RiskManager::new(limits_pct, 1000.0);

    let mut signal2 = create_test_signal("ETH/USDC");
    signal2.size = 1.0;
    signal2.cex_price = 200.0;
    let (allowed2, reason2) = manager_pct.check_pre_trade(&signal2);
    assert!(!allowed2, "Should be rejected due to % capital limit");
    assert!(reason2.contains("exceeds 10.0% of capital"));

    signal2.cex_price = 50.0;
    let (allowed3, _) = manager_pct.check_pre_trade(&signal2);
    assert!(allowed3, "Valid trade should pass");
}

#[test]
fn test_risk_manager_daily_loss_limit() {
    let limits = RiskLimits {
        max_trade_usd: 10_000.0,
        max_trade_pct: 1.0,
        max_daily_loss: 50.0,
        ..Default::default()
    };
    let mut manager = RiskManager::new(limits, 1000.0);
    let mut signal = create_test_signal("ETH/USDC");
    signal.size = 0.1;

    manager.record_trade(-30.0);
    assert!(
        manager.check_pre_trade(&signal).0,
        "Should allow trade after small loss"
    );

    manager.record_trade(-25.0);
    let (allowed, reason) = manager.check_pre_trade(&signal);
    assert!(!allowed, "Should reject after crossing daily loss limit");
    assert!(reason.contains("Daily loss limit"));
}

#[test]
fn test_risk_manager_consecutive_losses() {
    let limits = RiskLimits {
        max_trade_usd: 10_000.0,
        max_daily_loss: 1_000.0,
        max_trade_pct: 1.0,
        consecutive_loss_limit: 2,
        ..Default::default()
    };

    let mut manager = RiskManager::new(limits, 1000.0);
    let mut signal = create_test_signal("ETH/USDC");
    signal.size = 0.1;

    manager.record_trade(-10.0);
    assert!(manager.check_pre_trade(&signal).0);

    manager.record_trade(-10.0);
    let (allowed, reason) = manager.check_pre_trade(&signal);
    assert!(
        !allowed,
        "Should be rejected after hitting consecutive loss limit"
    );
    assert!(reason.contains("Consecutive loss limit"));

    manager.record_trade(50.0);
    assert!(
        manager.check_pre_trade(&signal).0,
        "Should be allowed after a win"
    );
}

#[test]
fn test_risk_manager_drawdown_limit() {
    let limits = RiskLimits {
        max_trade_usd: 10_000.0,
        max_trade_pct: 1.0,
        max_drawdown_pct: 0.20,
        ..Default::default()
    };

    let mut manager = RiskManager::new(limits, 1000.0);
    let mut signal = create_test_signal("ETH/USDC");
    signal.size = 0.1;

    manager.record_trade(500.0);

    manager.record_trade(-200.0);
    assert!(manager.check_pre_trade(&signal).0);

    manager.record_trade(-200.0);
    let (allowed, reason) = manager.check_pre_trade(&signal);
    assert!(!allowed);
    assert!(reason.contains("Drawdown"));
}

#[test]
fn test_risk_manager_frequency_limit() {
    let limits = RiskLimits {
        max_trade_usd: 10_000.0,
        max_trade_pct: 1.0,
        max_trades_per_hour: 3,
        ..Default::default()
    };

    let mut manager = RiskManager::new(limits, 1000.0);
    let mut signal = create_test_signal("ETH/USDC");
    signal.size = 0.1;

    for _ in 0..3 {
        assert!(manager.check_pre_trade(&signal).0);
        manager.record_trade(1.0);
    }

    let (allowed, reason) = manager.check_pre_trade(&signal);
    assert!(!allowed);
    assert!(reason.contains("Hourly trade limit"));
}

#[test]
fn test_risk_manager_reset_daily() {
    let limits = RiskLimits {
        max_trade_usd: 10_000.0,
        max_trade_pct: 1.0,
        max_trades_per_hour: 2,
        max_daily_loss: 10.0,
        ..Default::default()
    };

    let mut manager = RiskManager::new(limits, 1000.0);
    let mut signal = create_test_signal("ETH/USDC");
    signal.size = 0.1;

    manager.record_trade(1.0);
    manager.record_trade(1.0);
    assert!(!manager.check_pre_trade(&signal).0);

    manager.reset_daily();

    assert!(manager.check_pre_trade(&signal).0);
}

#[test]
fn test_validator_signal_sanity() {
    let validator = PreTradeValidator::default();
    let mut signal = create_test_signal("ETH/USDC");

    signal.timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let (ok, msg) = validator.validate_signal(&signal);
    assert!(ok, "Valid signal failed: {}", msg);

    let mut bad_price = signal.clone();
    bad_price.cex_price = 0.0;
    let (ok, msg) = validator.validate_signal(&bad_price);
    assert!(!ok);
    assert!(msg.contains("Invalid CEX price"));

    let mut bad_spread = signal.clone();
    bad_spread.spread_bps = 600.0;
    let (ok, msg) = validator.validate_signal(&bad_spread);
    assert!(!ok);
    assert!(msg.contains("too high"));

    let mut stale = signal.clone();
    stale.timestamp -= 10.0;
    let (ok, msg) = validator.validate_signal(&stale);
    assert!(!ok);
    assert!(msg.contains("Signal too old"));
}

#[test]
fn test_validator_price_feed_deviation() {
    let validator = PreTradeValidator::new(5);
    let pair = "ETH/USDC";

    validator.validate_price_feed(100.0, pair);
    validator.validate_price_feed(100.0, pair);
    validator.validate_price_feed(100.0, pair);

    let (ok, _) = validator.validate_price_feed(102.0, pair);
    assert!(ok);

    let (ok, msg) = validator.validate_price_feed(115.0, pair);
    assert!(!ok);
    assert!(msg.contains("deviates"));

    for _ in 0..5 {
        validator.validate_price_feed(200.0, pair);
    }
    let (ok, _) = validator.validate_price_feed(202.0, pair);
    assert!(ok, "Should pass against new higher average");
}

#[test]
fn test_monitoring_seconds_since() {
    let now = Instant::now();
    let result = monitoring::seconds_since(Some(now));
    assert!(result.is_some());
    assert!(result.unwrap() <= 1);

    let result_none = monitoring::seconds_since(None);
    assert!(result_none.is_none());
}

#[test]
fn test_bot_health_struct() {
    let health = monitoring::BotHealth {
        is_running: true,
        uptime_seconds: 3600,
        last_trade_age_seconds: Some(10),
        circuit_breaker_open: false,
        session_pnl: 125.50,
        daily_pnl_limit_reached: false,
    };

    assert_eq!(health.uptime_seconds, 3600);
    assert_eq!(health.session_pnl, 125.50);
    health.log();
}

#[test]
fn test_monitoring_logs_no_panic() {
    monitoring::log_trade("BTC/USDT", "BuyCexSellDex", 0.5, 45.0, 12.0, "DONE");
    monitoring::log_error("test_context", "Something went wrong");
}

#[tokio::test]
async fn test_kill_switch_capital_protection() {
    let telegram = Arc::new(TelegramAlert::new("".to_string()));
    let limits = RiskLimits::default();
    let risk_manager = RiskManager::new(limits, 1000.0);
    let mut kill_switch = AutoKillSwitch::new();

    let killed = kill_switch.check(&risk_manager, 0, &telegram).await;
    assert!(!killed);

    let mut bad_risk = RiskManager::new(RiskLimits::default(), 1000.0);
    bad_risk.record_trade(-600.0);

    let killed = kill_switch.check(&bad_risk, 0, &telegram).await;
    assert!(killed);
    assert!(kill_switch.triggered);
    assert!(kill_switch.reason.unwrap().contains("dropped below 50%"));
}

#[tokio::test]
async fn test_kill_switch_error_flood() {
    let telegram = Arc::new(TelegramAlert::new("".to_string()));
    let limits = RiskLimits::default();
    let risk_manager = RiskManager::new(limits, 1000.0);
    let mut kill_switch = AutoKillSwitch::new();

    let killed = kill_switch.check(&risk_manager, 60, &telegram).await;
    assert!(killed);
    assert!(kill_switch.triggered);
    assert!(
        kill_switch
            .reason
            .unwrap()
            .contains("Critical error frequency")
    );
}

#[tokio::test]
async fn test_telegram_alert_logic() {
    let alert = TelegramAlert::new("chat_id".to_string());

    alert.send("Unit test message", false).await;
    alert.send("Urgent unit test", true).await;

    let has_stop = alert.check_for_stop_command().await;
    assert!(!has_stop);
}

#[cfg(test)]
mod tests {
    use executor::gas_oracle::gas_units;

    use super::*;

    fn oracle_with_history() -> GasOracle {
        let mut o = GasOracle::new(30.0, 0.1, 3_000.0);
        for gwei in [28.0, 31.0, 29.0, 35.0, 27.0, 32.0] {
            o.update(gwei);
        }
        o
    }

    #[test]
    fn pessimistic_is_above_ewm() {
        let o = oracle_with_history();
        assert!(
            o.pessimistic_estimate() >= o.current_gwei(),
            "pessimistic estimate must be ≥ EWM mean"
        );
    }

    #[test]
    fn std_dev_is_positive_after_variance() {
        let o = oracle_with_history();
        assert!(o.std_dev() > 0.0);
    }

    #[test]
    fn net_profit_subtracts_gas() {
        let o = GasOracle::new(30.0, 0.1, 3_000.0);
        let (net, gas_cost) = o.net_profit_usd(20.0, gas_units::UNISWAP_V3_SWAP);
        assert!((gas_cost - 14.40).abs() < 0.01, "gas_cost={gas_cost}");
        assert!((net - (20.0 - 14.40)).abs() < 0.01, "net={net}");
    }

    #[test]
    fn safety_gate_rejects_negative_net() {
        let o = GasOracle::new(50.0, 0.1, 3_000.0);
        let result = safety_check_with_gas(5.0, gas_units::FLASH_LOAN_ARB, &o);
        assert!(result.is_err(), "should reject when gas > profit");
    }

    #[test]
    fn safety_gate_rejects_high_gas_ratio() {
        let o = GasOracle::new(100.0, 0.1, 4_000.0);
        let result = safety_check_with_gas(4.0, gas_units::UNISWAP_V2_SWAP, &o);
        assert!(result.is_err());
    }

    #[test]
    fn safety_gate_passes_profitable_trade() {
        let o = GasOracle::new(5.0, 0.1, 2_000.0);
        let result = safety_check_with_gas(10.0, gas_units::UNISWAP_V3_SWAP, &o);
        assert!(result.is_ok());
        let net = result.unwrap();
        assert!((net - 8.40).abs() < 0.01, "net={net}");
    }

    #[test]
    fn eth_price_update_changes_cost() {
        let mut o = GasOracle::new(30.0, 0.1, 3_000.0);
        let cost_before = o.estimated_cost_usd(gas_units::UNISWAP_V3_SWAP);
        o.set_eth_price(6_000.0);
        let cost_after = o.estimated_cost_usd(gas_units::UNISWAP_V3_SWAP);
        assert!((cost_after / cost_before - 2.0).abs() < 1e-9);
    }
}
