use approx::assert_relative_eq;
use arb_core::TokenAmount;
use chrono::Utc;
use inventory::pnl::{ArbRecord, PnLEngine, Side};
use inventory::rebalancer::{RebalancePlanner, TRANSFER_FEES};
use inventory::{
    pnl::TradeLeg,
    tracker::{InventoryTracker, Venue},
};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::fs;

fn create_mock_trade(
    id: &str,
    buy_price: Decimal,
    sell_price: Decimal,
    amount: Decimal,
) -> ArbRecord {
    let v_bin = Venue::Cex;
    let v_uni = Venue::Wallet;

    let buy = TradeLeg {
        id: format!("{id}_buy"),
        timestamp: Utc::now(),
        venue: v_uni,
        symbol: "ETH/USDT".to_string(),
        side: Side::Buy,
        amount,
        price: buy_price,
        fee: dec!(0.75),
        fee_asset: "USDT".to_string(),
    };

    let sell = TradeLeg {
        id: format!("{id}_sell"),
        timestamp: Utc::now(),
        venue: v_bin,
        symbol: "ETH/USDT".to_string(),
        side: Side::Sell,
        amount,
        price: sell_price,
        fee: dec!(0.4),
        fee_asset: "USDT".to_string(),
    };

    ArbRecord {
        id: id.to_string(),
        timestamp: Utc::now(),
        buy_leg: buy,
        sell_leg: sell,
        gas_cost_usd: dec!(1.5),
    }
}

#[test]
fn test_snapshot_aggregates_across_venues() {
    let mut tracker = InventoryTracker::new(None);

    let mut binance_balances = HashMap::new();
    binance_balances.insert("ETH".to_string(), (dec!(10.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, binance_balances);

    let wallet_balances =
        vec![TokenAmount::from_human("5.0", 18, Some("ETH".to_string())).unwrap()];
    tracker.update_from_wallet(wallet_balances);

    let snapshot = tracker.snapshot();

    let total_eth_raw = snapshot["totals"]["ETH"].to_string();

    let total_eth = total_eth_raw.trim_matches('"');

    assert_eq!(Decimal::from_str(total_eth).unwrap(), dec!(15.0));
}

#[test]
fn test_can_execute_passes_when_sufficient() {
    let mut tracker = InventoryTracker::new(None);

    let mut cex_bal = HashMap::new();
    cex_bal.insert("USDT".to_string(), (dec!(1000.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, cex_bal);

    let wallet_bal = vec![TokenAmount::from_human("1.0", 18, Some("ETH".to_string())).unwrap()];
    tracker.update_from_wallet(wallet_bal);

    let result = tracker.can_execute(
        Venue::Cex,
        "USDT",
        dec!(500.0),
        Venue::Wallet,
        "ETH",
        dec!(1.0),
    );

    assert!(result["can_execute"].as_bool().unwrap());
    assert!(result["reason"].is_null());
}

#[test]
fn test_can_execute_fails_insufficient_buy() {
    let mut tracker = InventoryTracker::new(None);

    let mut cex_bal = HashMap::new();
    cex_bal.insert("USDT".to_string(), (dec!(100.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, cex_bal);

    let wallet_bal = vec![TokenAmount::from_human("10.0", 18, Some("ETH".to_string())).unwrap()];
    tracker.update_from_wallet(wallet_bal);

    let result = tracker.can_execute(
        Venue::Cex,
        "USDT",
        dec!(500.0),
        Venue::Wallet,
        "ETH",
        dec!(1.0),
    );

    assert!(!result["can_execute"].as_bool().unwrap());
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("Insufficient USDT")
    );
}

#[test]
fn test_can_execute_fails_insufficient_sell() {
    let mut tracker = InventoryTracker::new(None);

    let mut cex_bal = HashMap::new();
    cex_bal.insert("USDT".to_string(), (dec!(10000.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, cex_bal);

    let wallet_bal = vec![TokenAmount::from_human("0.1", 18, Some("ETH".to_string())).unwrap()];
    tracker.update_from_wallet(wallet_bal);

    let result = tracker.can_execute(
        Venue::Cex,
        "USDT",
        dec!(500.0),
        Venue::Wallet,
        "ETH",
        dec!(1.0),
    );

    assert!(!result["can_execute"].as_bool().unwrap());
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("Insufficient ETH")
    );
}

#[test]
fn test_record_trade_updates_balances() {
    let mut tracker = InventoryTracker::new(None);

    let mut cex_bal = HashMap::new();
    cex_bal.insert("USDT".to_string(), (dec!(1000.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, cex_bal);

    tracker.record_trade(
        Venue::Cex,
        "buy",
        "ETH",
        "USDT",
        dec!(1.0),
        dec!(500.0),
        dec!(0.1),
        "USDT",
    );

    assert_eq!(tracker.get_available(Venue::Cex, "ETH"), dec!(1.0));
    assert_eq!(tracker.get_available(Venue::Cex, "USDT"), dec!(499.9));
}

#[test]
fn test_skew_detects_imbalance() {
    let mut tracker = InventoryTracker::new(None);

    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(80.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![TokenAmount::from_human("20.0", 18, Some("ETH".to_string())).unwrap()];
    tracker.update_from_wallet(w_bal);

    let skew_data = tracker.skew("ETH");

    assert!(
        !skew_data["needs_rebalance"].as_bool().unwrap()
            || skew_data["max_deviation_pct"].as_f64().unwrap() >= 0.3
    );
    let total_val = Decimal::from_str(&skew_data["total"].to_string())
        .or_else(|_| Decimal::from_f64(skew_data["total"].as_f64().unwrap_or(0.0)).ok_or("fail"))
        .expect("Could not parse total as Decimal");
    assert_eq!(total_val, dec!(100));
}

#[test]
fn test_skew_balanced() {
    let mut tracker = InventoryTracker::new(None);

    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(50.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![TokenAmount::from_human("50.0", 18, Some("ETH".to_string())).unwrap()];
    tracker.update_from_wallet(w_bal);

    let skew_data = tracker.skew("ETH");

    assert!(!skew_data["needs_rebalance"].as_bool().unwrap());
    assert_eq!(skew_data["max_deviation_pct"].as_f64().unwrap(), 0.0);
}

#[test]
fn test_check_detects_skewed_asset() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(8.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![TokenAmount::from_human("2.0", 18, Some("ETH".into())).unwrap()];
    tracker.update_from_wallet(w_bal);

    let planner = RebalancePlanner::new(&tracker, 30.0);
    let results = planner.check_all();

    let eth_report = results.iter().find(|r| r["asset"] == "ETH").unwrap();
    assert!(eth_report["needs_rebalance"].as_bool().unwrap());
    assert_relative_eq!(eth_report["max_deviation_pct"].as_f64().unwrap(), 0.3);
}

#[test]
fn test_check_passes_balanced_asset() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("USDT".to_string(), (dec!(550.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![TokenAmount::from_human("450.0", 6, Some("USDT".into())).unwrap()];
    tracker.update_from_wallet(w_bal);

    let planner = RebalancePlanner::new(&tracker, 30.0);
    let results = planner.check_all();

    let usdt_report = results.iter().find(|r| r["asset"] == "USDT").unwrap();
    assert!(!usdt_report["needs_rebalance"].as_bool().unwrap());
}

#[test]
fn test_plan_generates_correct_transfer() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(20.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![TokenAmount::from_human("80.0", 18, Some("ETH".into())).unwrap()];
    tracker.update_from_wallet(w_bal);

    let planner = RebalancePlanner::new(&tracker, 10.0);
    let plans = planner.plan("ETH");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].from_venue, Venue::Wallet);
    assert_eq!(plans[0].to_venue, Venue::Cex);
    assert_eq!(plans[0].amount, dec!(30.0));
}

#[test]
fn test_plan_respects_min_operating_balance() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(0.6), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let planner = RebalancePlanner::new(&tracker, 0.0);
    let plans = planner.plan("ETH");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].amount, dec!(0.1));
    assert_eq!(
        tracker.get_available(Venue::Cex, "ETH") - plans[0].amount,
        dec!(0.5)
    );
}

#[test]
fn test_plan_accounts_for_fees() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(10.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let planner = RebalancePlanner::new(&tracker, 10.0);
    let plans = planner.plan("ETH");

    let plan = &plans[0];
    let fee = TRANSFER_FEES.get("ETH").unwrap().withdrawal_fee;
    assert_eq!(plan.estimated_fee, fee);
    assert_eq!(plan.net_amount(), plan.amount - fee);
}

#[test]
fn test_plan_empty_when_balanced() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(5.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![TokenAmount::from_human("5.0", 18, Some("ETH".into())).unwrap()];
    tracker.update_from_wallet(w_bal);

    let planner = RebalancePlanner::new(&tracker, 30.0);
    let plans = planner.plan("ETH");

    assert!(plans.is_empty());
}

#[test]
fn test_estimate_cost_sums_correctly() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(10.0), dec!(0.0)));
    b_bal.insert("USDT".to_string(), (dec!(10000.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let planner = RebalancePlanner::new(&tracker, 0.0);
    let all_plans = planner.plan_all();

    let mut flat_plans = Vec::new();
    for p in all_plans.values() {
        flat_plans.extend(p.clone());
    }

    let summary = planner.estimate_cost(&flat_plans);

    assert_eq!(summary["total_transfers"], 2);
    assert!(summary["assets_affected"].as_array().unwrap().len() == 2);

    let eth_fee = TRANSFER_FEES.get("ETH").unwrap().withdrawal_fee;
    let usdt_fee = TRANSFER_FEES.get("USDT").unwrap().withdrawal_fee;

    assert_eq!(
        summary["total_fees"]["ETH"]
            .as_f64()
            .map(|f| Decimal::from_f64(f).unwrap())
            .expect("ETH fee should be a number"),
        eth_fee
    );
    assert_eq!(
        summary["total_fees"]["USDT"]
            .as_f64()
            .map(|f| Decimal::from_f64(f).unwrap())
            .expect("USDT fee should be a number"),
        usdt_fee
    );
}

#[test]
fn test_planner_respects_min_withdrawal() {
    let mut tracker = InventoryTracker::new(None);
    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(0.505), dec!(0.0)));
    tracker.update_from_cex(Venue::Cex, b_bal);

    let w_bal = vec![
        TokenAmount::from_human("0.495", 18, Some("ETH".to_string()))
            .expect("Failed to create ETH amount"),
    ];
    tracker.update_from_wallet(w_bal);

    let planner = RebalancePlanner::new(&tracker, 0.0);
    let plans = planner.plan("ETH");

    assert!(plans.is_empty());
}

#[test]
fn test_gross_pnl_calculation() {
    let trade = create_mock_trade("arb1", dec!(2400.0), dec!(2500.0), dec!(1.5));
    assert_eq!(trade.gross_pnl(), dec!(150.0));
}

#[test]
fn test_net_pnl_includes_all_fees() {
    let trade = create_mock_trade("arb1", dec!(2400.0), dec!(2500.0), dec!(1.5));
    assert_eq!(trade.net_pnl(), dec!(147.35));
}

#[test]
fn test_pnl_bps_calculation() {
    let trade = create_mock_trade("arb1", dec!(2400.0), dec!(2500.0), dec!(1.5));
    let expected_bps = (dec!(147.35) / dec!(3600.0)) * dec!(10000);
    assert_eq!(trade.net_pnl_bps(), expected_bps);
}

#[sqlx::test(migrations = "../migrations")]
async fn test_summary_win_rate(pool: sqlx::PgPool) {
    let engine = PnLEngine::new(pool);
    let _ = engine
        .record(create_mock_trade(
            "arb1",
            dec!(2400.0),
            dec!(2500.0),
            dec!(1.0),
        ))
        .await;
    let _ = engine
        .record(create_mock_trade(
            "arb2",
            dec!(2500.0),
            dec!(2500.0),
            dec!(1.0),
        ))
        .await;

    let summary = engine.summary().await.unwrap();
    assert_eq!(summary["total_trades"], "2");
    assert_eq!(summary["win_rate"], "50.0%");
}

#[sqlx::test(migrations = "../migrations")]
async fn test_summary_with_no_trades(pool: sqlx::PgPool) {
    let engine = PnLEngine::new(pool);
    let summary = engine.summary().await.unwrap();
    assert!(summary.is_empty());
}

#[sqlx::test(migrations = "../migrations")]
async fn test_export_csv_format(pool: sqlx::PgPool) {
    let engine = PnLEngine::new(pool);
    let _ = engine
        .record(create_mock_trade(
            "arb1",
            dec!(2400.0),
            dec!(2500.0),
            dec!(1.0),
        ))
        .await;

    let file_path = "test_pnl.csv";
    engine.export_csv(file_path).await.unwrap();

    let contents = fs::read_to_string(file_path).unwrap();
    assert!(contents.contains("id,timestamp,buy_venue,sell_venue,net_pnl,bps"));
    assert!(contents.contains("arb1"));

    fs::remove_file(file_path).unwrap();
}
