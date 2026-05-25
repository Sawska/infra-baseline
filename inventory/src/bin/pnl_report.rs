use arb_core::APP_CONFIG;
use chrono::Utc;
use colored::*;
use exchange::client::{ExchangeClient, ExchangeType, OrderBook};
use exchange::config::ExchangeConfig;
use inventory::pnl::{ArbRecord, PnLEngine, Side, TradeLeg};
use inventory::tracker::Venue;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

enum MarketUpdate {
    Binance(OrderBook),
    Bybit(OrderBook),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print!("{}[2J{}[1;1H", 27 as char, 27 as char);
    println!("🚀 Starting Real-Time Arbitrage Engine...");
    println!("   Connecting to Binance and Bybit streams...");

    let cfg_binance = ExchangeConfig::from_env(ExchangeType::Binance).unwrap();
    let client_binance = Arc::new(ExchangeClient::new(cfg_binance, ExchangeType::Binance).await?);

    let cfg_bybit = ExchangeConfig::from_env(ExchangeType::Bybit).unwrap();
    let client_bybit = Arc::new(ExchangeClient::new(cfg_bybit, ExchangeType::Bybit).await?);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let tx_bin = tx.clone();
    let tx_by = tx.clone();

    let c_bin = client_binance.clone();
    tokio::spawn(async move {
        let symbol = "ETHUSDT";
        if let Err(e) = c_bin
            .stream_order_book(symbol, move |ob| {
                let _ = tx_bin.send(MarketUpdate::Binance(ob));
            })
            .await
        {
            eprintln!("❌ Binance Stream Error: {}", e);
        }
    });

    let c_by = client_bybit.clone();
    tokio::spawn(async move {
        let symbol = "ETH/USDT";
        if let Err(e) = c_by
            .stream_order_book(symbol, move |ob| {
                let _ = tx_by.send(MarketUpdate::Bybit(ob));
            })
            .await
        {
            eprintln!("❌ Bybit Stream Error: {}", e);
        }
    });
    let pg_config = &APP_CONFIG.postgres;

    let pool = PgPoolOptions::new()
        .max_connections(pg_config.max_connections)
        .min_connections(pg_config.min_connections)
        .acquire_timeout(Duration::from_secs(pg_config.connect_timeout_seconds))
        .idle_timeout(Duration::from_secs(pg_config.idle_timeout_seconds))
        .connect(&pg_config.url)
        .await
        .unwrap();

    let engine = PnLEngine::new(pool);
    let mut ob_binance: Option<OrderBook> = None;
    let mut ob_bybit: Option<OrderBook> = None;
    let mut trade_count = 0;

    let mut ui_ticker = interval(Duration::from_millis(500));
    let mut needs_render = true;

    loop {
        tokio::select! {
                    Some(update) = rx.recv() => {
                        match update {
                            MarketUpdate::Binance(ob) => ob_binance = Some(ob),
                            MarketUpdate::Bybit(ob) => ob_bybit = Some(ob),
                        }

                        if let (Some(bin), Some(byb)) = (&ob_binance, &ob_bybit)
            && let Some(record) = check_arb(bin, byb, trade_count) {
            engine.record(record).await.unwrap();
            trade_count += 1;
            needs_render = true;
        }

                    }
                    _ = ui_ticker.tick() => {
                        if needs_render || trade_count == 0 {
                            render_dashboard(&engine, &ob_binance, &ob_bybit).await;
                            needs_render = false;
                        }
                    }
                }
    }
}

fn check_arb(bin: &OrderBook, byb: &OrderBook, id_counter: usize) -> Option<ArbRecord> {
    let fee_rate = dec!(0.001);
    let trade_amt = dec!(1.0);

    let cost_buy_bin = bin.best_ask.0 * trade_amt;
    let recv_sell_byb = byb.best_bid.0 * trade_amt;

    let cost_net = cost_buy_bin * (dec!(1.0) + fee_rate);
    let recv_net = recv_sell_byb * (dec!(1.0) - fee_rate);
    let pnl_1 = recv_net - cost_net;

    let cost_buy_byb = byb.best_ask.0 * trade_amt;
    let recv_sell_bin = bin.best_bid.0 * trade_amt;

    let cost_net_2 = cost_buy_byb * (dec!(1.0) + fee_rate);
    let recv_net_2 = recv_sell_bin * (dec!(1.0) - fee_rate);
    let pnl_2 = recv_net_2 - cost_net_2;

    let min_profit = dec!(0.50);

    if pnl_1 > min_profit {
        return Some(ArbRecord {
            id: format!("arb-{}", id_counter),
            timestamp: Utc::now(),
            buy_leg: TradeLeg {
                id: format!("b-{}", id_counter),
                timestamp: Utc::now(),
                venue: Venue::Cex,
                symbol: "ETH/USDT".into(),
                side: Side::Buy,
                amount: trade_amt,
                price: bin.best_ask.0,
                fee: cost_buy_bin * fee_rate,
                fee_asset: "USDT".into(),
            },
            sell_leg: TradeLeg {
                id: format!("s-{}", id_counter),
                timestamp: Utc::now(),
                venue: Venue::Wallet,
                symbol: "ETH/USDT".into(),
                side: Side::Sell,
                amount: trade_amt,
                price: byb.best_bid.0,
                fee: recv_sell_byb * fee_rate,
                fee_asset: "USDT".into(),
            },
            gas_cost_usd: dec!(0.0),
        });
    }

    if pnl_2 > min_profit {
        return Some(ArbRecord {
            id: format!("arb-{}", id_counter),
            timestamp: Utc::now(),
            buy_leg: TradeLeg {
                id: format!("b-{}", id_counter),
                timestamp: Utc::now(),
                venue: Venue::Wallet,
                symbol: "ETH/USDT".into(),
                side: Side::Buy,
                amount: trade_amt,
                price: byb.best_ask.0,
                fee: cost_buy_byb * fee_rate,
                fee_asset: "USDT".into(),
            },
            sell_leg: TradeLeg {
                id: format!("s-{}", id_counter),
                timestamp: Utc::now(),
                venue: Venue::Cex,
                symbol: "ETH/USDT".into(),
                side: Side::Sell,
                amount: trade_amt,
                price: bin.best_bid.0,
                fee: recv_sell_bin * fee_rate,
                fee_asset: "USDT".into(),
            },
            gas_cost_usd: dec!(0.0),
        });
    }

    None
}

async fn render_dashboard(
    engine: &PnLEngine,
    ob_bin: &Option<OrderBook>,
    ob_by: &Option<OrderBook>,
) {
    let s = engine.summary().await.unwrap();

    print!("{}[2J{}[1;1H", 27 as char, 27 as char);

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║             LIVE ARBITRAGE MONITOR (PAPER MODE)              ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  LIVE PRICES (ETH/USDT)                                      ║");

    let bin_str = if let Some(ob) = ob_bin {
        format!("${:.2} / ${:.2}", ob.best_bid.0, ob.best_ask.0)
    } else {
        "Waiting...".to_string()
    };

    let byb_str = if let Some(ob) = ob_by {
        format!("${:.2} / ${:.2}", ob.best_bid.0, ob.best_ask.0)
    } else {
        "Waiting...".to_string()
    };

    println!("║  Binance: {:<42} ║", bin_str.cyan());
    println!("║  Bybit:   {:<42} ║", byb_str.magenta());
    println!("╠══════════════════════════════════════════════════════════════╣");

    if s.is_empty() {
        println!("║  Waiting for opportunities...                                ║");
        println!("╚══════════════════════════════════════════════════════════════╝");
        return;
    }

    println!(
        "║  Total Opps:   {:<10} | Win Rate: {:<18} ║",
        s["total_trades"], s["win_rate"]
    );
    println!(
        "║  Potential PnL:${:<9} | Avg BPS:  {:<18} ║",
        s["total_pnl_usd"], s["avg_pnl_bps"]
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  RECENT OPPORTUNITIES                                        ║");

    let recent = engine.recent(5).await.unwrap();
    for t in recent {
        let status = if t["is_win"] == "true" { "✅" } else { "❌" };
        let sign = if t["pnl"].starts_with('-') { "" } else { "+" };

        let route = t["route"]
            .as_str()
            .replace("Wallet", "Bybit")
            .replace("Cex", "Binance");
        let route_display = if route.len() > 24 {
            format!("{}..", &route[0..22])
        } else {
            route
        };

        println!(
            "║  {}  {:<4} {:<24} {}${:<6} ({:>5} bps) {} ║",
            t["time"], t["asset"], route_display, sign, t["pnl"], t["bps"], status
        );
    }
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("  Monitoring for spreads > 0.1% (net of fees)...");
}
