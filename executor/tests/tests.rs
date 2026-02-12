use arb_core::Address;
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use executor::engine::{Executor, ExecutorConfig, ExecutorState};
use executor::recovery::CircuitBreakerConfig;
use inventory::tracker::InventoryTracker;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use strategy::signal::{Direction, Signal};
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Helper to setup an Executor for testing
async fn setup_executor(config: ExecutorConfig) -> Executor {
    let ex_config = ExchangeConfig {
        api_key: "test".to_string(),
        secret: "test".to_string(),
        is_sandbox: true,
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
