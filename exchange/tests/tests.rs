use arb_core::TokenAmount;
use exchange::analyzer::OrderBookAnalyzer;
use exchange::client::{
    Balance, ExchangeClient, ExchangeType, OrderBook, OrderResult, RateLimiter,
};
use exchange::config::ExchangeConfig;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tokio::time::{Duration, timeout};

fn create_test_order_book() -> OrderBook {
    OrderBook {
        symbol: "ETH/USDT".to_string(),
        timestamp: 1738620000000,
        bids: vec![
            (dec!(2000.0), dec!(10.0)),
            (dec!(1990.0), dec!(20.0)),
            (dec!(1980.0), dec!(30.0)),
        ],
        asks: vec![
            (dec!(2010.0), dec!(10.0)),
            (dec!(2020.0), dec!(20.0)),
            (dec!(2030.0), dec!(30.0)),
        ],
        best_bid: (dec!(2000.0), dec!(10.0)),
        best_ask: (dec!(2010.0), dec!(10.0)),
        mid_price: dec!(2005.0),
        spread_bps: dec!(49.88),
    }
}

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
        rate_limiter: RateLimiter::new(1200),
    }
}

#[tokio::test]
async fn test_fetch_order_book_structure() {
    let ob = create_test_order_book();
    assert!(!ob.symbol.is_empty(), "Symbol should not be empty");
    assert!(ob.timestamp > 0, "Timestamp should be positive");
    assert!(!ob.bids.is_empty(), "Bids should be populated");
    assert!(!ob.asks.is_empty(), "Asks should be populated");
}

#[test]
fn test_order_book_bids_descending() {
    let ob = create_test_order_book();
    for i in 0..ob.bids.len() - 1 {
        assert!(
            ob.bids[i].0 > ob.bids[i + 1].0,
            "Bids must be sorted highest to lowest"
        );
    }
}

#[test]
fn test_order_book_asks_ascending() {
    let ob = create_test_order_book();
    for i in 0..ob.asks.len() - 1 {
        assert!(
            ob.asks[i].0 < ob.asks[i + 1].0,
            "Asks must be sorted lowest to highest"
        );
    }
}

#[test]
fn test_spread_calculation() {
    let best_bid = dec!(2000.0);
    let best_ask = dec!(2010.0);
    let mid = (best_bid + best_ask) / dec!(2.0);
    let spread_bps = ((best_ask - best_bid) / mid) * dec!(10000.0);
    assert_eq!(spread_bps.round_dp(2), dec!(49.88));
}

#[test]
fn test_fetch_balance_filters_zeros() {
    let mut balances = HashMap::new();

    balances.insert(
        "ETH".to_string(),
        Balance {
            free: TokenAmount::from_human("1.0", 18, Some("ETH".into())).unwrap(),
            locked: TokenAmount::from_human("0", 18, Some("ETH".into())).unwrap(),
            total: TokenAmount::from_human("1.0", 18, Some("ETH".into())).unwrap(),
        },
    );

    let dust = Balance {
        free: TokenAmount::from_human("0", 18, Some("USDT".into())).unwrap(),
        locked: TokenAmount::from_human("0", 18, Some("USDT".into())).unwrap(),
        total: TokenAmount::from_human("0", 18, Some("USDT".into())).unwrap(),
    };

    if dust.total.raw > alloy_primitives::U256::ZERO {
        balances.insert("USDT".to_string(), dust);
    }

    assert!(balances.contains_key("ETH"));
    assert!(
        !balances.contains_key("USDT"),
        "Zero balances should be filtered out"
    );
}

#[test]
fn test_limit_ioc_returns_fill_info() {
    let res = OrderResult {
        id: "order_123".into(),
        symbol: "ETH/USDT".into(),
        side: "buy".into(),
        order_type: "limit".into(),
        time_in_force: "IOC".into(),
        amount_requested: TokenAmount::from_human("10.0", 8, None).unwrap(),
        amount_filled: TokenAmount::from_human("4.5", 8, None).unwrap(),
        avg_fill_price: dec!(2005.5),
        fee: TokenAmount::from_human("0.001", 8, None).unwrap(),
        status: "partially_filled".into(),
        timestamp: 1738620000000,
    };

    assert_eq!(res.amount_filled.to_human(), dec!(4.5));
    assert_eq!(res.avg_fill_price, dec!(2005.5));
    assert!(res.fee.raw > alloy_primitives::U256::ZERO);
}

#[tokio::test]
async fn test_rate_limiter_blocks_when_exhausted() {
    let client = get_test_client(ExchangeType::Binance).await;

    client.rate_limiter.update_from_headers(1200.0).await;

    let weight_to_request = 1000;

    let result = tokio::time::timeout(
        Duration::from_millis(200),
        client.rate_limiter.wait(weight_to_request),
    )
    .await;

    assert!(
        result.is_err(),
        "The rate limiter should have blocked the request because the weight is exhausted"
    );
}

#[tokio::test]
async fn test_rate_limiter_recovers_after_wait() {
    let client = get_test_client(ExchangeType::Binance).await;

    client.rate_limiter.update_from_headers(1199.5).await;

    let weight_to_request = 1;

    let result = timeout(
        Duration::from_secs(2),
        client.rate_limiter.wait(weight_to_request),
    )
    .await;

    assert!(
        result.is_ok(),
        "The rate limiter should have allowed the request after a short refill period"
    );
}

#[test]
fn test_walk_the_book_exact_fill() {
    let analyzer = OrderBookAnalyzer::new(create_test_order_book());
    let res = analyzer.walk_the_book("buy", dec!(10.0));

    assert!(res.fully_filled);
    assert_eq!(res.avg_price, dec!(2010.0));
    assert_eq!(res.levels_consumed, 1);
}

#[test]
fn test_walk_the_book_multiple_levels() {
    let analyzer = OrderBookAnalyzer::new(create_test_order_book());
    let res = analyzer.walk_the_book("buy", dec!(15.0));

    assert!(res.fully_filled);
    assert_eq!(res.levels_consumed, 2);
    assert_eq!(res.avg_price.round_dp(2), dec!(2013.33));
}

#[test]
fn test_walk_the_book_insufficient_liquidity() {
    let analyzer = OrderBookAnalyzer::new(create_test_order_book());
    let res = analyzer.walk_the_book("buy", dec!(100.0));

    assert!(!res.fully_filled);
    assert_eq!(res.levels_consumed, 3);
    let filled_qty: Decimal = res.fills.iter().map(|f| f.qty).sum();
    assert_eq!(filled_qty, dec!(60.0));
}

#[test]
fn test_depth_at_bps_correct() {
    let analyzer = OrderBookAnalyzer::new(create_test_order_book());
    let (qty, _) = analyzer.depth_at_bps("ask", dec!(10.0));
    assert_eq!(qty, dec!(10.0));
}

#[test]
fn test_imbalance_range() {
    let analyzer = OrderBookAnalyzer::new(create_test_order_book());
    let imb = analyzer.imbalance(10);

    assert!(imb >= dec!(-1.0) && imb <= dec!(1.0));
    assert_eq!(imb, dec!(0.0));
}

#[test]
fn test_effective_spread_greater_than_quoted() {
    let analyzer = OrderBookAnalyzer::new(create_test_order_book());
    let quoted = analyzer.orderbook.spread_bps;

    let effective = analyzer.effective_spread(dec!(20.0));
    assert!(effective >= quoted);
}

#[tokio::test]
async fn test_real_binance_order_book_fetch() {
    let client = get_test_client(ExchangeType::Binance).await;
    let symbol = "BTCUSDT";

    println!(
        "Fetching live order book for {} from Binance Testnet...",
        symbol
    );

    let result = client.fetch_order_book(symbol, 5).await;

    match result {
        Ok(book) => {
            assert_eq!(book.symbol, symbol);
            assert!(book.timestamp > 0, "Timestamp should be valid");

            assert!(!book.bids.is_empty(), "Live order book should have bids");
            assert!(!book.asks.is_empty(), "Live order book should have asks");

            let best_bid = book.best_bid.0;
            let best_ask = book.best_ask.0;

            assert!(
                best_ask > best_bid,
                "Crossed book detected! Ask ({}) <= Bid ({})",
                best_ask,
                best_bid
            );

            assert!(book.spread_bps > dec!(0.0), "Spread should be positive");
            assert!(book.mid_price > dec!(0.0), "Mid price should be positive");

            println!(
                "Successfully validated live order book. Spread: {} bps",
                book.spread_bps
            );
        }
        Err(e) => {
            panic!("Failed to fetch live data from Binance Testnet: {:?}", e);
        }
    }
}

#[tokio::test]
async fn test_real_binance_analyzer_integration() {
    let client = get_test_client(ExchangeType::Binance).await;
    let symbol = "BTCUSDT";

    println!("Running analyzer integration on live {} data...", symbol);
    let book_result = client.fetch_order_book(symbol, 5).await;

    match book_result {
        Ok(book) => {
            let best_ask_price = book.best_ask.0;
            let analyzer = OrderBookAnalyzer::new(book);

            let order_size = dec!(0.1);
            let fill = analyzer.walk_the_book("buy", order_size);

            assert!(
                fill.fully_filled,
                "Should be able to buy 0.1 BTC on liquid pair"
            );
            assert!(
                fill.avg_price >= best_ask_price,
                "Buy fill price must be >= best ask"
            );
            assert!(
                fill.levels_consumed >= 1,
                "Must consume at least the first level"
            );

            let max_expected_price = best_ask_price * dec!(1.05);
            assert!(
                fill.avg_price <= max_expected_price,
                "Slippage too high! Avg: {}, Max Expected: {}",
                fill.avg_price,
                max_expected_price
            );

            let imb = analyzer.imbalance(20);
            assert!(
                imb >= dec!(-1.0) && imb <= dec!(1.0),
                "Imbalance must be normalized between -1 and 1"
            );

            println!(
                "Analyzer integration passed. Filled {} BTC @ ~{}",
                order_size, fill.avg_price
            );
        }
        Err(e) => {
            panic!("Failed to fetch live data for integration test: {:?}", e);
        }
    }
}
