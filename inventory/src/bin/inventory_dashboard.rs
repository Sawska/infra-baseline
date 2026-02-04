use inventory::tracker::{InventoryTracker, Venue};
use rust_decimal_macros::dec;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut tracker = InventoryTracker::new(None);

    tracker.update_from_cex(Venue::Binance, {
        let mut m = std::collections::HashMap::new();
        m.insert("ETH".to_string(), (dec!(5.5), dec!(0.0)));
        m.insert("USDT".to_string(), (dec!(12000.0), dec!(0.0)));
        m
    });

    loop {
        print!("{}[2J{}[1;1H", 27 as char, 27 as char);

        let snapshot = tracker.snapshot();
        let timestamp = snapshot["timestamp"].as_str().unwrap_or("Unknown");

        println!("╔══════════════════════════════════════════════════════════════╗");
        println!("║                REAL-TIME INVENTORY DASHBOARD                 ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║ Last Updated: {:<46} ║", timestamp);
        println!("╠══════════════════════════════════════════════════════════════╣");

        for (venue_name, assets) in snapshot["venues"].as_object().unwrap() {
            println!("║ VENUE: {:<53} ║", venue_name);
            println!("║ ──────────────────────────────────────────────────────────── ║");
            for (asset, balance) in assets.as_object().unwrap() {
                let free = balance["free"].as_str().unwrap_or("0");
                let locked = balance["locked"].as_str().unwrap_or("0");
                println!(
                    "║   {:<5} | Free: {:>15} | Locked: {:>12} ║",
                    asset, free, locked
                );
            }
            println!("╠══════════════════════════════════════════════════════════════╣");
        }

        println!("║ PORTFOLIO TOTALS (All Venues)                                ║");
        for (asset, total) in snapshot["totals"].as_object().unwrap() {
            println!("║   {:<10}: {:>43} ║", asset, total);
        }

        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║ REBALANCE SKEW ANALYSIS                                      ║");
        let eth_skew = tracker.skew("ETH");
        let needs_reb = if eth_skew["needs_rebalance"].as_bool().unwrap_or(false) {
            "⚠️  YES"
        } else {
            "✅ NO"
        };
        println!(
            "║   ETH Skew Dev: {:>7.2}% | Needs Rebalance: {:>13} ║",
            eth_skew["max_deviation_pct"].as_f64().unwrap_or(0.0) * 100.0,
            needs_reb
        );

        println!("╚══════════════════════════════════════════════════════════════╝");
        println!("  (Press Ctrl+C to exit)");

        sleep(Duration::from_secs(2)).await;
    }
}
