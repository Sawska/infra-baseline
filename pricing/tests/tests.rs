use alloy_primitives::B256;
use alloy_primitives::U256;
use alloy_rpc_types::Transaction;
use arb_chain::ChainClient;
use arb_core::Address;
use pricing::amm::{Pool, Token, UniswapV2Pair, UniswapV3Pool};
use pricing::engine::PricingEngine;
use pricing::monitor::MempoolMonitor;
use pricing::router::RouteFinder;
use pricing::simulator::ForkSimulator;
use serde_json::json;
use std::sync::Arc;

fn make_addr(byte: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[19] = byte;
    Address(alloy_primitives::Address::from(bytes))
}

fn make_token(byte: u8, symbol: &str) -> Token {
    Token::new(make_addr(byte), 18, symbol.to_string())
}

fn make_pair(t0: &Token, t1: &Token, r0: u128, r1: u128) -> UniswapV2Pair {
    let dec = U256::from(10).pow(U256::from(18));
    UniswapV2Pair::new(
        make_addr(t0.address.0[19] + t1.address.0[19]),
        t0.clone(),
        t1.clone(),
        U256::from(r0) * dec,
        U256::from(r1) * dec,
        30,
    )
}

#[test]
fn test_get_amount_out_basic() {
    let token_eth = Token::new(make_addr(1), 18, "ETH".to_string());
    let token_usdc = Token::new(make_addr(2), 6, "USDC".to_string());

    let reserve_eth = U256::from(1000) * U256::from(10).pow(U256::from(18));
    let reserve_usdc = U256::from(2_000_000) * U256::from(10).pow(U256::from(6));

    let pair = UniswapV2Pair::new(
        make_addr(3),
        token_eth.clone(),
        token_usdc.clone(),
        reserve_eth,
        reserve_usdc,
        30,
    );

    let amount_in_usdc = U256::from(2000) * U256::from(10).pow(U256::from(6));

    let eth_out = pair.get_amount_out(amount_in_usdc, &token_usdc).unwrap();

    let one_eth = U256::from(10).pow(U256::from(18));

    assert!(
        eth_out < one_eth,
        "Output should be less than 1 ETH (fees/impact)"
    );

    let min_eth = one_eth * U256::from(99) / U256::from(100);
    assert!(eth_out > min_eth, "Output too low, math might be wrong");
}

#[test]
fn test_get_amount_out_matches_solidity() {
    let token_in = Token::new(make_addr(10), 18, "IN".to_string());
    let token_out = Token::new(make_addr(11), 18, "OUT".to_string());

    let reserve_in = U256::from(100_000);
    let reserve_out = U256::from(50_000);
    let amount_in = U256::from(1_000);
    let fee_bps = 30;

    let pair = UniswapV2Pair::new(
        make_addr(12),
        token_in.clone(),
        token_out.clone(),
        reserve_in,
        reserve_out,
        fee_bps,
    );

    let calculated_out = pair.get_amount_out(amount_in, &token_in).unwrap();

    assert_eq!(calculated_out, U256::from(493));
}

#[test]
fn test_integer_math_no_floats() {
    let token0 = Token::new(make_addr(20), 18, "T0".to_string());
    let token1 = Token::new(make_addr(21), 18, "T1".to_string());

    let huge_reserve = U256::from(10).pow(U256::from(30));

    let pair = UniswapV2Pair::new(
        make_addr(22),
        token0.clone(),
        token1.clone(),
        huge_reserve,
        huge_reserve,
        30,
    );

    let amount_in = U256::from(10).pow(U256::from(25));

    let amount_out = pair.get_amount_out(amount_in, &token0).unwrap();

    assert!(amount_out > U256::ZERO);

    assert!(amount_out < amount_in);
}

#[test]
fn test_swap_is_immutable() {
    let token0 = Token::new(make_addr(30), 18, "T0".to_string());
    let token1 = Token::new(make_addr(31), 18, "T1".to_string());

    let reserve_initial = U256::from(1_000_000);

    let pair = UniswapV2Pair::new(
        make_addr(32),
        token0.clone(),
        token1.clone(),
        reserve_initial,
        reserve_initial,
        30,
    );

    let amount_in = U256::from(1000);

    let new_pair = pair.simulate_swap(amount_in, &token0).unwrap();

    assert_eq!(pair.reserve0, reserve_initial);
    assert_eq!(pair.reserve1, reserve_initial);

    assert_eq!(new_pair.reserve0, reserve_initial + amount_in);

    assert!(new_pair.reserve1 < reserve_initial);

    assert_eq!(new_pair.address, pair.address);
    assert_eq!(new_pair.fee_bps, pair.fee_bps);
}

#[test]
fn test_direct_vs_multihop() {
    let token_a = make_token(1, "A");
    let token_b = make_token(2, "B");
    let token_c = make_token(3, "C");

    let pair_ac = make_pair(&token_a, &token_c, 1_000, 1_000);

    let pair_ab = make_pair(&token_a, &token_b, 100_000, 100_000);
    let pair_bc = make_pair(&token_b, &token_c, 100_000, 100_000);

    let finder = RouteFinder::new(vec![
        Pool::V2(pair_ac.clone()),
        Pool::V2(pair_ab.clone()),
        Pool::V2(pair_bc.clone()),
    ]);

    let amount_in = U256::from(100) * U256::from(10).pow(U256::from(18));

    let (best_route, output) = finder
        .find_best_route(&token_a, &token_c, amount_in, 0, 3)
        .unwrap();

    assert_eq!(
        best_route.num_hops(),
        2,
        "Should pick multi-hop due to better liquidity"
    );
    assert!(output > U256::ZERO);

    let direct_out = pair_ac.get_amount_out(amount_in, &token_a).unwrap();
    assert!(output > direct_out, "Multi-hop should yield more output");
}

#[test]
fn test_gas_makes_direct_better() {
    let token_a = make_token(1, "A");
    let token_b = make_token(2, "B");
    let token_c = make_token(3, "WETH");

    let pair_ac = make_pair(&token_a, &token_c, 99_000, 99_000);

    let pair_ab = make_pair(&token_a, &token_b, 100_000, 100_000);
    let pair_bc = make_pair(&token_b, &token_c, 100_000, 100_000);

    let finder = RouteFinder::new(vec![
        Pool::V2(pair_ac.clone()),
        Pool::V2(pair_ab.clone()),
        Pool::V2(pair_bc.clone()),
    ]);

    let amount_in = U256::from(10) * U256::from(10).pow(U256::from(18));

    let (best_route, _) = finder
        .find_best_route(&token_a, &token_c, amount_in, 500, 3)
        .unwrap();

    assert_eq!(
        best_route.num_hops(),
        1,
        "Should pick direct due to high gas costs"
    );
}

#[test]
fn test_no_route_exists() {
    let token_a = make_token(1, "A");
    let token_b = make_token(2, "B");
    let token_c = make_token(3, "C");

    let pair_ab = make_pair(&token_a, &token_b, 10_000, 10_000);

    let finder = RouteFinder::new(vec![Pool::V2(pair_ab)]);

    let result = finder.find_best_route(&token_a, &token_c, U256::from(100), 0, 3);

    assert!(result.is_err(), "Should fail to find route");
}

#[test]
fn test_route_output_matches_sequential_swaps() {
    let token_a = make_token(1, "A");
    let token_b = make_token(2, "B");
    let token_c = make_token(3, "C");

    let pair_ab = make_pair(&token_a, &token_b, 100_000, 100_000);
    let pair_bc = make_pair(&token_b, &token_c, 100_000, 100_000);

    let finder = RouteFinder::new(vec![Pool::V2(pair_ab.clone()), Pool::V2(pair_bc.clone())]);

    let amount_in = U256::from(1000) * U256::from(10).pow(U256::from(18));

    let (route, route_out) = finder
        .find_best_route(&token_a, &token_c, amount_in, 0, 3)
        .unwrap();

    let out_b = pair_ab.get_amount_out(amount_in, &token_a).unwrap();
    let out_c = pair_bc.get_amount_out(out_b, &token_b).unwrap();

    assert_eq!(
        route_out, out_c,
        "Route simulation must equal sequential swap output"
    );
    assert_eq!(route.path.len(), 3);
}

#[test]
fn test_mempool_parsing_handles_malformed() {
    let monitor = MempoolMonitor::new("ws://invalid".to_string(), |_| async {});

    let make_tx = |input_hex: &str| -> Transaction {
        serde_json::from_value(json!({
            "hash": B256::ZERO,
            "nonce": 0,
            "input": input_hex,
            "from": Address::ZERO,
            "to": Address::ZERO,
            "value": "0x0",
            "gas": 21000,
            "gasPrice": "0x1",
            "type": "0x0",
            "v": "0x1c",
            "r": "0x1",
            "s": "0x1"
        }))
        .expect("Failed to build test tx")
    };

    let tx = make_tx("0x");
    assert!(
        monitor.parse_transaction(&tx).is_none(),
        "Empty input should return None"
    );

    let tx = make_tx("0x0102");
    assert!(
        monitor.parse_transaction(&tx).is_none(),
        "Short input should return None"
    );

    let tx = make_tx("0xffffffff");
    assert!(
        monitor.parse_transaction(&tx).is_none(),
        "Unknown selector should return None"
    );
}

#[tokio::test]
async fn test_simulation_matches_calculation() {
    let fork_url = std::env::var("RPC_URL_LOCAL").unwrap_or_default();
    if fork_url.is_empty() {
        println!("Skipping simulation test: RPC_URL_LOCAL not set");
        return;
    }
    let simulator = ForkSimulator::new(&fork_url).expect("Failed to create simulator");

    let token_in = make_token(1, "WETH");
    let token_out = make_token(2, "USDC");
    let pair = make_pair(&token_in, &token_out, 10_000, 20_000_000);

    let router_addr = make_addr(99);
    let sender = make_addr(100);
    let amount_in = U256::from(1000);

    let result = simulator
        .compare_simulation_vs_calculation(
            &Pool::V2(pair),
            router_addr.0,
            amount_in,
            &token_in,
            sender.0,
        )
        .await
        .expect("Simulation vs Calculation comparison failed to execute");

    assert!(
        result.matches,
        "Calculation ({}) did not match Simulation ({}). Difference: {}",
        result.calculated, result.simulated, result.difference
    );

    assert!(
        result.difference < U256::from(1),
        "Difference exceeds tolerance"
    );
}

#[tokio::test]
async fn test_pricing_engine_integration() {
    let fork_url = std::env::var("RPC_URL_LOCAL").unwrap_or_default();

    let client = Arc::new(ChainClient::new("http://localhost:8545"));

    let mut engine = PricingEngine::new(
        client,
        if fork_url.is_empty() {
            "http://localhost:8545"
        } else {
            &fork_url
        },
        "ws://localhost:8546",
        |_| async {},
    )
    .unwrap();

    let t_a = make_token(1, "A");
    let t_b = make_token(2, "B");
    let pair = make_pair(&t_a, &t_b, 100_000, 100_000);

    engine.pools.insert(pair.address, Pool::V2(pair.clone()));
    engine.router = Some(RouteFinder::new(vec![Pool::V2(pair.clone())]));

    let quote_res = engine
        .get_quote(
            &t_a,
            &t_b,
            U256::from(100),
            10,
            make_addr(99),
            make_addr(100),
        )
        .await;

    match quote_res {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("Simulation failed") || msg.contains("connection refused"),
                "Unexpected error type: {}",
                msg
            );
        }
    }
}

#[test]
fn test_uniswap_v3_math() {
    let t0 = make_token(1, "USDC");
    let t1 = make_token(2, "ETH");

    let sqrt_price_x96 = U256::from(1) << 96;
    let liquidity = 1_000_000_000_000_000_000u128;

    let pool = UniswapV3Pool::new(
        make_addr(3),
        t0.clone(),
        t1.clone(),
        liquidity,
        sqrt_price_x96,
        0,
        3000,
        60,
    );

    let amount_in = U256::from(1000);
    let amount_out = pool.get_amount_out(amount_in, &t0).unwrap();

    assert!(amount_out < U256::from(1000));
    assert!(amount_out > U256::from(990));
}
#[test]
fn test_arbitrage_detection() {
    let t_a = make_token(1, "A");
    let t_b = make_token(2, "B");
    let t_c = make_token(3, "C");

    let e18 = U256::from(10).pow(U256::from(18));

    let p1 = make_pair(&t_a, &t_b, 100_000, 200_000);

    let p2 = make_pair(&t_b, &t_c, 100_000, 100_000);

    let p3 = make_pair(&t_c, &t_a, 100_000, 100_000);

    let finder = RouteFinder::new(vec![Pool::V2(p1), Pool::V2(p2), Pool::V2(p3)]);

    let amount_in = U256::from(100) * e18;

    let loops = finder.find_arbitrage_loops(&t_a, amount_in, 4);

    assert!(!loops.is_empty(), "Should find arbitrage loop");

    let best_loop = &loops[0];
    let output = best_loop.get_output(amount_in).unwrap();

    assert!(output > amount_in, "Arbitrage should be profitable");
}

#[tokio::test]
async fn test_engine_arbitrage_scan() {
    let fork_url = std::env::var("RPC_URL_LOCAL").unwrap_or_default();

    let client = Arc::new(ChainClient::new("http://localhost:8545"));

    let mut engine = PricingEngine::new(
        client,
        if fork_url.is_empty() {
            "http://localhost:8545"
        } else {
            &fork_url
        },
        "ws://localhost:8546",
        |_| async {},
    )
    .unwrap();

    let t_a = make_token(1, "A");
    let t_b = make_token(2, "B");

    let pair = make_pair(&t_a, &t_b, 100_000, 100_000);
    engine.pools.insert(pair.address, Pool::V2(pair.clone()));
    engine.router = Some(RouteFinder::new(vec![Pool::V2(pair.clone())]));

    let opportunities = engine
        .scan_for_arbitrage(&t_a, U256::from(100), make_addr(99), make_addr(100))
        .await;

    assert!(opportunities.is_empty());
}

#[test]
fn test_router_picks_lower_fee_pool() {
    let t_a = make_token(1, "A");
    let t_b = make_token(2, "B");

    let reserves = U256::from(1_000_000) * U256::from(10).pow(U256::from(18));

    let pool_high_fee = UniswapV2Pair::new(
        make_addr(10),
        t_a.clone(),
        t_b.clone(),
        reserves,
        reserves,
        30,
    );

    let pool_low_fee = UniswapV2Pair::new(
        make_addr(11),
        t_a.clone(),
        t_b.clone(),
        reserves,
        reserves,
        5,
    );

    let finder = RouteFinder::new(vec![
        Pool::V2(pool_high_fee.clone()),
        Pool::V2(pool_low_fee.clone()),
    ]);

    let amount_in = U256::from(1000) * U256::from(10).pow(U256::from(18));
    let (route, output) = finder.find_best_route(&t_a, &t_b, amount_in, 0, 1).unwrap();

    let out_high = pool_high_fee.get_amount_out(amount_in, &t_a).unwrap();
    let out_low = pool_low_fee.get_amount_out(amount_in, &t_a).unwrap();

    assert!(out_low > out_high, "Low fee pool should yield more");
    assert_eq!(output, out_low, "Router should pick the best yield");

    assert_eq!(
        route.pools[0].address(),
        pool_low_fee.address,
        "Route should go through low fee pool"
    );
}

#[test]
fn test_v3_swap_direction_inverses() {
    let t0 = make_token(1, "USDC");
    let t1 = make_token(2, "ETH");

    let sqrt_price_x96 = U256::from(1) << 96;
    let liquidity = 1_000_000_000_000_000_000u128;

    let pool = UniswapV3Pool::new(
        make_addr(3),
        t0.clone(),
        t1.clone(),
        liquidity,
        sqrt_price_x96,
        0,
        3000,
        60,
    );

    let amount_in = U256::from(10_000);

    let out_t1 = pool.get_amount_out(amount_in, &t0).unwrap();

    let out_t0 = pool.get_amount_out(out_t1, &t1).unwrap();

    assert!(out_t1 < amount_in, "Fees should reduce output");
    assert!(
        out_t0 < out_t1,
        "Fees should reduce output again on way back"
    );

    assert!(
        out_t0 > U256::from(9900),
        "Round trip loss should only be fees"
    );
}

#[test]
fn test_zero_input_handling() {
    let t_a = make_token(1, "A");
    let t_b = make_token(2, "B");

    let pair = make_pair(&t_a, &t_b, 100_000, 100_000);

    let out = pair.get_amount_out(U256::ZERO, &t_a).unwrap();
    assert_eq!(out, U256::ZERO, "Zero input should return zero output");

    let finder = RouteFinder::new(vec![Pool::V2(pair)]);

    let result = finder.find_best_route(&t_a, &t_b, U256::ZERO, 0, 3);

    if let Ok((_, val)) = result {
        assert_eq!(val, U256::ZERO);
    }
}

#[tokio::test]
async fn test_matches_uniswap_v2_exact_on_chain() {
    let fork_url = std::env::var("RPC_URL_LOCAL").unwrap_or_default();
    if fork_url.is_empty() {
        println!("Skipping Uniswap exact match test: RPC_URL_LOCAL not set");
        return;
    }

    let simulator = ForkSimulator::new(&fork_url).expect("Failed to create simulator");

    let pair_address: Address =
        Address::from_string("0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc").unwrap();
    let router_address =
        Address::from_string("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D").unwrap();
    let token_eth = make_token(1, "WETH");
    let token_usdc = Token::new(make_addr(2), 6, "USDC".to_string());

    let (r0, r1) = simulator
        .get_v2_reserves(pair_address.0)
        .await
        .expect("Failed to fetch on-chain reserves");

    let pair_model = UniswapV2Pair::new(
        pair_address,
        token_eth.clone(),
        token_usdc.clone(),
        r0,
        r1,
        30,
    );

    let amount_in = U256::from(1) * U256::from(10).pow(U256::from(18));

    let local_output = pair_model.get_amount_out(amount_in, &token_eth).unwrap();

    let chain_output = simulator
        .call_uniswap_v2_get_amount_out(router_address.0, amount_in, r0, r1)
        .await
        .expect("Failed to call Uniswap on-chain");

    assert_eq!(
        local_output, chain_output,
        "Local math (result: {}) must match Uniswap V2 Solidity (result: {}) exactly",
        local_output, chain_output
    );
}
