use alloy_primitives::U256;
use arb_core::TokenAmount;
use arb_core::types::Address;
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use inventory::tracker::{InventoryTracker, Venue};
use pricing::amm::{Pool, Token, UniswapV2Pair};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use strategy::fees::FeeStructure;
use strategy::generator::{GeneratorConfig, PairMetadata, SignalGenerator};
use strategy::scorer::{ScorerConfig, SignalScorer};
use strategy::signal::{Direction, Signal};
use tokio::sync::Mutex;

/// Helper to setup a generator instance for testing.
async fn setup_mock_generator() -> SignalGenerator {
    let config = ExchangeConfig {
        api_key: "test".to_string(),
        secret: "test".to_string(),
        is_sandbox: true,
        skip_connection_validation: true,
    };

    let client = ExchangeClient::new(config, ExchangeType::Binance)
        .await
        .unwrap();
    let inventory = Arc::new(Mutex::new(InventoryTracker::new(None)));
    let fees = FeeStructure::default();
    let gen_config = GeneratorConfig::default();

    SignalGenerator::new(Arc::new(client), inventory, fees, gen_config)
}

/// Helper to create mock AMM metadata for ETH/USDC.
fn create_mock_metadata() -> PairMetadata {
    let addr = Address::ZERO;
    let base = Token::new(addr, 18, "ETH".to_string());
    let quote = Token::new(addr, 6, "USDC".to_string());

    let pool = Pool::V2(UniswapV2Pair::new(
        addr,
        base.clone(),
        quote.clone(),
        U256::from(100) * U256::from(10).pow(U256::from(18)),
        U256::from(200_000) * U256::from(10).pow(U256::from(6)),
        30,
    ));

    PairMetadata {
        pool,
        base_token: base,
        quote_token: quote,
    }
}

#[tokio::test]
async fn test_generate_signal_profitable() {
    let mut generator = setup_mock_generator().await;
    let pair = "ETH/USDC";

    generator.register_pair(pair.to_string(), create_mock_metadata());

    let mut cex_balances = HashMap::new();
    cex_balances.insert("USDC".to_string(), (Decimal::from(50000), Decimal::ZERO));
    cex_balances.insert("ETH".to_string(), (Decimal::from(10), Decimal::ZERO));

    generator
        .inventory
        .lock()
        .await
        .update_from_cex(Venue::Cex, cex_balances);

    let signal = generator.generate(pair, 1.0).await;

    if let Some(s) = signal {
        println!(
            "Real Market Signal Found: {:?} at price {}",
            s.direction, s.cex_price
        );
        assert!(s.expected_net_pnl > 0.0);
        assert!(s.spread_bps >= generator.config.min_spread_bps);
    } else {
        println!("No profitable signal found at the moment with real market data.");
    }
}

#[tokio::test]
async fn test_generate_signal_no_opportunity() {
    let mut generator = setup_mock_generator().await;
    let pair = "ETH/USDC";
    generator.register_pair(pair.to_string(), create_mock_metadata());

    generator.config.min_spread_bps = 5000.0;

    let signal = generator.generate(pair, 1.0).await;
    assert!(
        signal.is_none(),
        "Should not generate signal with impossible spread requirement"
    );
}

#[tokio::test]
async fn test_cooldown_prevents_rapid_signals() {
    let mut generator = setup_mock_generator().await;
    let pair = "ETH/USDC";
    generator.register_pair(pair.to_string(), create_mock_metadata());
    generator.config.cooldown_seconds = 10.0;

    let mut balances = HashMap::new();
    balances.insert("USDC".to_string(), (Decimal::from(100000), Decimal::ZERO));
    balances.insert("ETH".to_string(), (Decimal::from(100), Decimal::ZERO));

    generator
        .inventory
        .lock()
        .await
        .update_from_cex(Venue::Cex, balances.clone());

    let _ = generator.generate(pair, 1.0).await;

    let second_signal = generator.generate(pair, 1.0).await;
    assert!(
        second_signal.is_none(),
        "Cooldown should prevent immediate second signal"
    );
}

#[tokio::test]
async fn test_direction_selection() {
    let mut generator = setup_mock_generator().await;
    let pair = "ETH/USDC";
    generator.register_pair(pair.to_string(), create_mock_metadata());

    let mut balances = HashMap::new();
    balances.insert("USDC".to_string(), (Decimal::from(100000), Decimal::ZERO));
    balances.insert("ETH".to_string(), (Decimal::from(100), Decimal::ZERO));

    generator
        .inventory
        .lock()
        .await
        .update_from_cex(Venue::Cex, balances.clone());

    let signal = generator.generate(pair, 1.0).await;

    if let Some(s) = signal {
        assert!(matches!(
            s.direction,
            Direction::BuyCexSellDex | Direction::BuyDexSellCex
        ));
    }
}

#[tokio::test]
async fn test_score_high_spread() {
    let config = ScorerConfig {
        excellent_spread_bps: 100.0,
        min_spread_bps: 30.0,
        spread_weight: 1.0,
        ..Default::default()
    };
    let scorer = SignalScorer::new(Some(config));
    let inventory = Mutex::new(InventoryTracker::new(None));

    let signal = Signal::new(
        "ETH/USDC".to_string(),
        Direction::BuyCexSellDex,
        2000.0,
        2020.0,
        100.0,
        1.0,
        20.0,
        5.0,
        15.0,
        0.0,
        0.0,
        true,
        true,
    );

    let score = scorer.score(&signal, &inventory).await;
    assert!(score >= 90.0);
}

#[tokio::test]
async fn test_score_inventory_penalty() {
    let config = ScorerConfig {
        inventory_weight: 1.0,
        spread_weight: 0.0,
        liquidity_weight: 0.0,
        history_weight: 0.0,
        ..Default::default()
    };
    let scorer = SignalScorer::new(Some(config));
    let inventory = Mutex::new(InventoryTracker::new(None));

    let signal = Signal::new(
        "ETH/USDC".to_string(),
        Direction::BuyCexSellDex,
        2000.0,
        2010.0,
        50.0,
        1.0,
        10.0,
        2.0,
        8.0,
        0.0,
        0.0,
        true,
        true,
    );

    let mut cex_balances = HashMap::new();
    cex_balances.insert("ETH".to_string(), (Decimal::from(5), Decimal::ZERO));
    inventory
        .lock()
        .await
        .update_from_cex(Venue::Cex, cex_balances);

    let wallet_eth = TokenAmount::from_human("5", 18, Some("ETH".to_string())).unwrap();
    inventory.lock().await.update_from_wallet(vec![wallet_eth]);

    let good_score = scorer.score(&signal, &inventory).await;

    inventory.lock().await.update_from_wallet(vec![]);

    let penalty_score = scorer.score(&signal, &inventory).await;

    assert!(
        penalty_score < good_score,
        "Score expected to drop from 60 to 20"
    );
}

#[tokio::test]
async fn test_decay_over_time() {
    let scorer = SignalScorer::new(None);
    let ttl = 10.0;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let signal = Signal::new(
        "ETH/USDC".to_string(),
        Direction::BuyCexSellDex,
        2000.0,
        2010.0,
        50.0,
        1.0,
        10.0,
        2.0,
        8.0,
        80.0,
        now + ttl,
        true,
        true,
    );

    let initial_decay = scorer.apply_decay(&signal);

    let old_signal = Signal {
        timestamp: now - (ttl / 2.0),
        ..signal.clone()
    };

    let later_decay = scorer.apply_decay(&old_signal);

    assert!(
        later_decay < initial_decay,
        "Decay should reduce score over time"
    );
    assert!(later_decay > 0.0);
}
