use arb_core::TokenAmount;
use clap::{Parser, Subcommand};
use colored::*;
use inventory::rebalancer::RebalancePlanner;
use inventory::tracker::{InventoryTracker, Venue};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use std::collections::HashMap;

#[derive(Parser)]
#[command(name = "rebalancer")]
#[command(about = "Inventory Rebalance Planner CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check current inventory skew
    Check,
    /// Generate a rebalance plan for an asset
    Plan { asset: String },
}

fn main() {
    let cli = Cli::parse();

    let mut tracker = InventoryTracker::new(None);

    let mut b_bal = HashMap::new();
    b_bal.insert("ETH".to_string(), (dec!(2.0), dec!(0.0)));
    b_bal.insert("USDT".to_string(), (dec!(18000.0), dec!(0.0)));
    tracker.update_from_cex(Venue::Binance, b_bal);

    let w_bal = vec![
        TokenAmount::from_human("8.0", 18, Some("ETH".to_string())).expect("Invalid ETH amount"),
        TokenAmount::from_human("12000.0", 6, Some("USDT".to_string()))
            .expect("Invalid USDT amount"),
    ];
    tracker.update_from_wallet(w_bal);

    let planner = RebalancePlanner::new(&tracker, 30.0);

    match &cli.command {
        Commands::Check => {
            println!("\n{}", "Inventory Skew Report".bold().underline());
            println!("{}", "═══════════════════════════════════════════".blue());

            for skew in planner.check_all() {
                let asset = skew["asset"].as_str().unwrap();
                println!("Asset: {}", asset.yellow().bold());

                for venue in ["Binance", "Wallet"] {
                    let data = &skew["venues"][venue];
                    let amt = data["amount"].as_f64().unwrap();
                    let pct = data["pct"].as_f64().unwrap() * 100.0;
                    let dev = data["deviation_pct"].as_f64().unwrap() * 100.0;

                    let dev_str = if dev > 5.0 {
                        format!("← deviation: {:+>.0}%", dev).red()
                    } else {
                        "".normal()
                    };

                    println!(
                        "  {:<8}  {:.1} {}  ({:.0}%)   {}",
                        venue, amt, asset, pct, dev_str
                    );
                }

                let status = if skew["needs_rebalance"].as_bool().unwrap() {
                    "⚠️  NEEDS REBALANCE".red().bold()
                } else {
                    "✅  OK".green()
                };
                println!("  Status: {}\n", status);
            }
            println!("{}", "═══════════════════════════════════════════".blue());
        }
        Commands::Plan { asset } => {
            let plans = planner.plan(asset);
            if plans.is_empty() {
                println!("No rebalance plan needed for {}.", asset);
                return;
            }

            let eth_price = dec!(2000.0);

            println!("\n{} {}", "Rebalance Plan:".bold(), asset.yellow());
            println!("{}", "───────────────────────────────────────────".blue());

            let mut total_fee_usd = Decimal::ZERO;

            for (i, p) in plans.iter().enumerate() {
                let fee_usd = p.estimated_fee * eth_price;
                total_fee_usd += fee_usd;

                println!("Transfer {}:", i + 1);
                println!("  From:     {:?}", p.from_venue);
                println!("  To:       {:?}", p.to_venue);
                println!(
                    "  Amount:   {} {}",
                    p.amount.to_string().bright_white(),
                    p.asset
                );
                println!(
                    "  Fee:      {} {} (~${})",
                    p.estimated_fee,
                    p.asset,
                    fee_usd.round_dp(2)
                );
                println!("  ETA:      ~{} min", p.estimated_time_min);

                let total_inventory = tracker.get_available(Venue::Binance, asset)
                    + tracker.get_available(Venue::Wallet, asset);
                let target = total_inventory / dec!(2.0);

                println!("\n  Result:");
                println!("    Binance:  {:.1} {} (50%)", target, p.asset);
                println!("    Wallet:   {:.1} {} (50%)", target, p.asset);
            }

            println!(
                "\nEstimated total cost: ${}",
                total_fee_usd.round_dp(2).to_string().green()
            );
        }
    }
}
