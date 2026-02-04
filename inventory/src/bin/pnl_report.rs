use chrono::Utc;
use inventory::pnl::{ArbRecord, PnLEngine, Side, TradeLeg};
use inventory::tracker::Venue;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    let show_chart = args.contains(&"--chart".to_string());

    let mut engine = PnLEngine::new();
    let v_bin = Venue::Cex;
    let v_uni = Venue::Wallet;

    for i in 0..25 {
        let is_win = i % 4 != 0;
        let price_diff = if is_win {
            dec!(4.5) + (Decimal::from(i) * dec!(0.1))
        } else {
            dec!(-2.0)
        };

        let timestamp = Utc::now() - chrono::Duration::minutes((30 - i) * 15);

        let buy = TradeLeg {
            id: format!("b{}", i),
            timestamp,
            venue: v_uni,
            symbol: "ETH/USDT".into(),
            side: Side::Buy,
            amount: dec!(1.5),
            price: dec!(2400.0),
            fee: dec!(0.75),
            fee_asset: "USDT".into(),
        };

        let sell = TradeLeg {
            id: format!("s{}", i),
            timestamp,
            venue: v_bin,
            symbol: "ETH/USDT".into(),
            side: Side::Buy,
            amount: dec!(1.5),
            price: dec!(2400.0) + price_diff,
            fee: dec!(0.4),
            fee_asset: "USDT".into(),
        };

        engine.record(ArbRecord {
            id: format!("arb{}", i),
            timestamp,
            buy_leg: buy,
            sell_leg: sell,
            gas_cost_usd: dec!(1.5),
        });
    }

    if show_chart {
        println!("📊 Generating historical PnL chart...");
        match engine.export_plotly_html("pnl_history.html") {
            Ok(_) => println!("✅ Chart exported to pnl_history.html"),
            Err(e) => eprintln!("❌ Error exporting chart: {}", e),
        }
    }

    render_dashboard(&engine);
}

/// Renders a terminal-based dashboard UI.
fn render_dashboard(engine: &PnLEngine) {
    let s = engine.summary();
    if s.is_empty() {
        println!("No trades recorded yet.");
        return;
    }

    print!("{}[2J{}[1;1H", 27 as char, 27 as char);

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                REAL-TIME ARBITRAGE DASHBOARD                 ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  OVERVIEW (Last 24h)                                         ║");
    println!("║  ──────────────────                                          ║");
    println!(
        "║  Total Trades: {:<10} | Win Rate: {:<18} ║",
        s["total_trades"], s["win_rate"]
    );
    println!(
        "║  Net PnL:      ${:<9} | Avg BPS:  {:<18} ║",
        s["total_pnl_usd"], s["avg_pnl_bps"]
    );
    println!(
        "║  Sharpe:       {:<10} | Notional: ${:<17} ║",
        s["sharpe_estimate"], s["total_notional"]
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  PERFORMANCE EXTREMES                                        ║");
    println!(
        "║  Best:  ${:<14} | Worst: ${:<20} ║",
        s["best_trade_pnl"], s["worst_trade_pnl"]
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  RECENT ACTIVITY                                             ║");

    let recent = engine.recent(8);
    for t in recent {
        let status = if t["is_win"] == "true" { "✅" } else { "❌" };
        let sign = if t["pnl"].starts_with('-') { "" } else { "+" };
        println!(
            "║  {}  {:<4} {:<24} {}${:<6} ({:>5} bps) {} ║",
            t["time"], t["asset"], t["route"], sign, t["pnl"], t["bps"], status
        );
    }
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("  (Run with --chart to export historical Plotly chart)");
}
