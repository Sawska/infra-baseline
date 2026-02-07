use alloy_primitives::U256;
use arb_core::TokenAmount;
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use inventory::tracker::{InventoryTracker, Venue};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("🔌 Connecting to Exchanges...");

    println!("   > Initializing Binance...");
    let config_bin = ExchangeConfig::from_env(ExchangeType::Binance)
        .map_err(|e| anyhow::anyhow!("Binance Config Error: {:?}", e))?;
    let client_bin = ExchangeClient::new(config_bin, ExchangeType::Binance)
        .await
        .map_err(|e| anyhow::anyhow!("Binance Connect Error: {:?}", e))?;

    println!("   > Initializing Bybit...");
    let config_by = ExchangeConfig::from_env(ExchangeType::Bybit)
        .map_err(|e| anyhow::anyhow!("Bybit Config Error: {:?}", e))?;
    let client_by = ExchangeClient::new(config_by, ExchangeType::Bybit)
        .await
        .map_err(|e| anyhow::anyhow!("Bybit Connect Error: {:?}", e))?;

    let mut tracker = InventoryTracker::new(None);
    tracker.balances.insert(Venue::Wallet, HashMap::new());

    println!("✅ All Systems Online! Starting inventory loop...");
    sleep(Duration::from_secs(1)).await;

    loop {
        let mut aggregated_cex_map: HashMap<String, (Decimal, Decimal)> = HashMap::new();

        match client_bin.fetch_balance().await {
            Ok(balances) => {
                for (asset, bal) in balances {
                    let entry = aggregated_cex_map
                        .entry(asset)
                        .or_insert((Decimal::ZERO, Decimal::ZERO));
                    entry.0 += bal.free.to_human();
                    entry.1 += bal.locked.to_human();
                }
            }
            Err(e) => eprintln!("⚠️  Binance fetch error: {:?}", e),
        }

        match client_by.fetch_balance().await {
            Ok(balances) => {
                for (asset, bal) in balances {
                    let entry = aggregated_cex_map
                        .entry(asset)
                        .or_insert((Decimal::ZERO, Decimal::ZERO));
                    entry.0 += bal.free.to_human();
                    entry.1 += bal.locked.to_human();
                }
            }
            Err(e) => eprintln!("⚠️  Bybit fetch error: {:?}", e),
        }

        aggregated_cex_map.insert("ETH".to_string(), (dec!(10.0), dec!(0.0)));
        aggregated_cex_map.insert("USDT".to_string(), (dec!(5000.0), dec!(0.0)));

        tracker.update_from_cex(Venue::Cex, aggregated_cex_map);

        let wallet_balances = vec![
            TokenAmount {
                symbol: Some("ETH".to_string()),
                raw: U256::from(2_000_000_000_000_000_000u128),
                decimals: 18,
            },
            TokenAmount {
                symbol: Some("BTC".to_string()),
                raw: U256::from(50_000_000u128),
                decimals: 8,
            },
        ];
        tracker.update_from_wallet(wallet_balances);

        render_tui(&tracker);

        sleep(Duration::from_secs(2)).await;
    }
}

fn render_tui(tracker: &InventoryTracker) {
    print!("{}[2J{}[1;1H", 27 as char, 27 as char);

    let snapshot = tracker.snapshot();
    let timestamp = snapshot["timestamp"].as_str().unwrap_or("Unknown");

    let whitelist = ["ETH", "BTC", "USDT", "USDC", "BNB"];

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                REAL-TIME INVENTORY DASHBOARD                 ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Last Updated: {:<46} ║", timestamp);
    println!("╠══════════════════════════════════════════════════════════════╣");

    if let Some(venues) = snapshot["venues"].as_object() {
        for (venue_name, assets) in venues {
            println!("║ VENUE: {:<53} ║", venue_name);
            println!("║ ──────────────────────────────────────────────────────────── ║");

            if let Some(asset_map) = assets.as_object() {
                let mut displayed_count = 0;

                for (asset, balance) in asset_map {
                    if !whitelist.contains(&asset.as_str()) {
                        continue;
                    }

                    let get_val = |key: &str| -> String {
                        let v = &balance[key];
                        if let Some(s) = v.as_str() {
                            s.to_string()
                        } else if let Some(f) = v.as_f64() {
                            f.to_string()
                        } else {
                            "0".to_string()
                        }
                    };

                    let free = get_val("free");
                    let locked = get_val("locked");

                    // Filter out dust/empty balances for cleaner UI
                    let free_f = free.parse::<f64>().unwrap_or(0.0);
                    let locked_f = locked.parse::<f64>().unwrap_or(0.0);

                    if free_f > 0.0 || locked_f > 0.0 {
                        println!(
                            "║   {:<5} | Free: {:>15} | Locked: {:>12} ║",
                            asset, free, locked
                        );
                        displayed_count += 1;
                    }
                }

                if displayed_count == 0 {
                    println!("║   (No watched assets found)                                  ║");
                }
            }
            println!("╠══════════════════════════════════════════════════════════════╣");
        }
    }

    println!("║ PORTFOLIO TOTALS (All Venues)                                ║");
    if let Some(totals) = snapshot["totals"].as_object() {
        for (asset, total) in totals {
            if !whitelist.contains(&asset.as_str()) {
                continue;
            }

            let total_val = if let Some(f) = total.as_f64() {
                f
            } else {
                total.as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0)
            };

            if total_val > 0.0 {
                let display = if let Some(s) = total.as_str() {
                    s.to_string()
                } else {
                    total.to_string()
                };
                println!("║   {:<10}: {:>43} ║", asset, display);
            }
        }
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
}
