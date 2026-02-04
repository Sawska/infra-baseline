use anyhow::Result;
use arb_chain::ChainClient;
use arb_core::Address;
use exchange::client::{ExchangeClient, ExchangeType, RateLimiter};
use exchange::config::ExchangeConfig;
use integration::checker::ArbChecker;
use inventory::pnl::PnLEngine;
use inventory::tracker::{InventoryTracker, Venue};
use pricing::amm::Token;
use pricing::engine::PricingEngine;
use rust_decimal_macros::dec;
use std::sync::Arc;

const WETH_ADDR: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
const USDT_ADDR: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

/// Helper to create a test exchange client with a high rate limit for testing.
async fn get_test_client(exchange: ExchangeType) -> ExchangeClient {
    let config = ExchangeConfig {
        api_key: "test_key".into(),
        secret: "test_secret".into(),
        is_sandbox: true,
    };

    let (base_url, ws_url) = match exchange {
        ExchangeType::Binance => (
            "https://testnet.binance.vision".into(),
            "wss://testnet.binance.vision/ws".into(),
        ),
        ExchangeType::Bybit => (
            "https://api-testnet.bybit.com".into(),
            "wss://stream-testnet.bybit.com/v5/public".into(),
        ),
    };

    ExchangeClient {
        config,
        exchange_type: exchange,
        http_client: reqwest::Client::new(),
        base_url,
        ws_url,
        rate_limiter: RateLimiter::new(10000),
        used_weight: Arc::new(tokio::sync::Mutex::new(0)),
    }
}

/// Helper to initialize a pricing engine and load mock pools.
async fn get_test_pricing_engine()
-> Result<PricingEngine<impl Fn(pricing::monitor::MonitorEvent) -> std::future::Ready<()>>> {
    let fork_url =
        std::env::var("RPC_URL_LOCAL").unwrap_or_else(|_| "http://localhost:8545".to_string());
    let chain_client = Arc::new(ChainClient::new(&fork_url));

    let mut engine = PricingEngine::new(
        chain_client.clone(),
        &fork_url,
        "ws://localhost:8546",
        |_| std::future::ready(()),
    )?;

    let pools = vec![
        Address::from_string("0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852").unwrap(),
        Address::from_string("0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc").unwrap(),
        Address::from_string("0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11").unwrap(),
    ];
    engine.load_pools(pools).await?;

    Ok(engine)
}

#[tokio::test]
async fn test_arb_check_profitable_with_inventory() -> Result<()> {
    let pricing_engine = get_test_pricing_engine().await?;
    let exchange_client = get_test_client(ExchangeType::Binance).await;

    let mut tracker = InventoryTracker::new(None);
    tracker.update_from_cex(Venue::Binance, {
        let mut m = std::collections::HashMap::new();
        m.insert("ETH".to_string(), (dec!(10.0), dec!(0.0)));
        m.insert("USDT".to_string(), (dec!(25000.0), dec!(0.0)));
        m
    });

    let checker = ArbChecker::new(pricing_engine, exchange_client, tracker, PnLEngine::new());

    let eth = Token::new(
        Address::from_string(WETH_ADDR).unwrap(),
        18,
        "ETH".to_string(),
    );
    let usdt = Token::new(
        Address::from_string(USDT_ADDR).unwrap(),
        6,
        "USDT".to_string(),
    );
    let size = dec!(1.5);

    let opp = checker.check("ETH/USDT", size, &eth, &usdt).await?;

    if opp.estimated_net_pnl_bps > dec!(0) && opp.inventory_ok {
        assert!(
            opp.executable,
            "Arb should be executable when profitable and inventory exists"
        );
        println!("✅ Profitable: {} bps gap detected", opp.gap_bps);
    }

    Ok(())
}

#[tokio::test]
async fn test_arb_check_rejects_unprofitable_gap() -> Result<()> {
    let pricing_engine = get_test_pricing_engine().await?;
    let exchange_client = get_test_client(ExchangeType::Binance).await;

    let mut tracker = InventoryTracker::new(None);
    tracker.update_from_cex(Venue::Binance, {
        let mut m = std::collections::HashMap::new();
        m.insert("ETH".to_string(), (dec!(10.0), dec!(0.0)));
        m.insert("USDT".to_string(), (dec!(25000.0), dec!(0.0)));
        m
    });

    let checker = ArbChecker::new(pricing_engine, exchange_client, tracker, PnLEngine::new());
    let eth = Token::new(
        Address::from_string(WETH_ADDR).unwrap(),
        18,
        "ETH".to_string(),
    );
    let usdt = Token::new(
        Address::from_string(USDT_ADDR).unwrap(),
        6,
        "USDT".to_string(),
    );
    let size = dec!(1.0);

    let opp = checker.check("ETH/USDT", size, &eth, &usdt).await?;

    if opp.estimated_net_pnl_bps <= dec!(0) {
        assert!(
            !opp.executable,
            "Arb must be rejected if costs exceed market gap"
        );
        println!(
            "❌ Rejected: Net profit is {} bps",
            opp.estimated_net_pnl_bps
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_arb_check_rejects_without_inventory() -> Result<()> {
    let pricing_engine = get_test_pricing_engine().await?;
    let exchange_client = get_test_client(ExchangeType::Binance).await;

    let tracker = InventoryTracker::new(None);

    let checker = ArbChecker::new(pricing_engine, exchange_client, tracker, PnLEngine::new());
    let eth = Token::new(
        Address::from_string(WETH_ADDR).unwrap(),
        18,
        "ETH".to_string(),
    );
    let usdt = Token::new(
        Address::from_string(USDT_ADDR).unwrap(),
        6,
        "USDT".to_string(),
    );
    let size = dec!(1.0);

    let opp = checker.check("ETH/USDT", size, &eth, &usdt).await?;

    assert!(
        !opp.inventory_ok,
        "Inventory should be reported as insufficient"
    );
    assert!(
        !opp.executable,
        "Arb cannot be executable without necessary funds"
    );

    Ok(())
}

#[tokio::test]
async fn test_arb_check_route_impact_on_profitability() -> Result<()> {
    let pricing_engine = get_test_pricing_engine().await?;
    let exchange_client = get_test_client(ExchangeType::Binance).await;

    let mut tracker = InventoryTracker::new(None);
    tracker.update_from_cex(Venue::Binance, {
        let mut m = std::collections::HashMap::new();
        m.insert("ETH".to_string(), (dec!(1000.0), dec!(0.0)));
        m.insert("USDT".to_string(), (dec!(1000000.0), dec!(0.0)));
        m
    });

    let checker = ArbChecker::new(pricing_engine, exchange_client, tracker, PnLEngine::new());
    let eth = Token::new(
        Address::from_string(WETH_ADDR).unwrap(),
        18,
        "ETH".to_string(),
    );
    let usdt = Token::new(
        Address::from_string(USDT_ADDR).unwrap(),
        6,
        "USDT".to_string(),
    );

    let small_size = dec!(0.01);
    let large_size = dec!(800.0);

    let small_opp = checker.check("ETH/USDT", small_size, &eth, &usdt).await?;
    let large_opp = checker.check("ETH/USDT", large_size, &eth, &usdt).await?;

    assert!(
        large_opp.details.dex_price_impact_bps >= small_opp.details.dex_price_impact_bps,
        "Price impact should scale with trade size"
    );

    if large_opp.details.dex_price_impact_bps > dec!(100.0) {
        assert!(
            !large_opp.executable,
            "Large trade should be disqualified due to excessive DEX impact"
        );
        println!(
            "⚠️  High Slippage: {} bps impact for {} ETH",
            large_opp.details.dex_price_impact_bps, large_size
        );
    }

    Ok(())
}
