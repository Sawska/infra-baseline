use chrono::Utc;
use clap::Parser;
use colored::*;
use exchange::analyzer::OrderBookAnalyzer;
use exchange::client::ExchangeClient;
use exchange::config::ExchangeConfig;
use rust_decimal::prelude::*;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    symbol: String,

    #[arg(short, long, default_value_t = 20)]
    depth: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let config = ExchangeConfig::from_env(exchange::client::ExchangeType::Binance)
        .map_err(|e| anyhow::anyhow!("Config error: {:?}", e))?;
    let client = ExchangeClient::new(config, exchange::client::ExchangeType::Binance)
        .await
        .map_err(|e| anyhow::anyhow!("Client error: {:?}", e))?;

    println!("Fetching order book for {}...", args.symbol.yellow());
    let ob = client
        .fetch_order_book(&args.symbol, args.depth)
        .await
        .map_err(|e| anyhow::anyhow!("Fetch error: {:?}", e))?;

    let analyzer = OrderBookAnalyzer::new(ob.clone());
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    let (bid_depth_10, bid_cost_10) = analyzer.depth_at_bps("bid", Decimal::new(10, 0));
    let (ask_depth_10, ask_cost_10) = analyzer.depth_at_bps("ask", Decimal::new(10, 0));
    let imb = analyzer.imbalance(10);

    let walk_2 = analyzer.walk_the_book("buy", Decimal::new(2, 0));
    let walk_10 = analyzer.walk_the_book("buy", Decimal::new(10, 0));
    let eff_spread = analyzer.effective_spread(Decimal::new(2, 0));

    let base_asset = args.symbol.split('/').next().unwrap_or("UNIT");
    let border_color = |s: &str| s.blue();

    macro_rules! print_line {
        ($label:expr, $value:expr) => {
            println!("║ {:<20} {:<33} ║", $label, $value);
        };
    }

    println!(
        "{}",
        border_color("╔══════════════════════════════════════════════════════╗")
    );
    println!(
        "║  {:<52} ║",
        format!("{} Order Book Analysis", args.symbol)
            .bright_white()
            .bold()
    );
    println!("║  Timestamp: {:<40} ║", now.dimmed());
    println!(
        "{}",
        border_color("╠══════════════════════════════════════════════════════╣")
    );

    print_line!(
        "Best Bid:",
        format!(
            "${:<12.2} × {:>10.4} {}",
            ob.best_bid.0, ob.best_bid.1, base_asset
        )
        .green()
    );
    print_line!(
        "Best Ask:",
        format!(
            "${:<12.2} × {:>10.4} {}",
            ob.best_ask.0, ob.best_ask.1, base_asset
        )
        .red()
    );
    print_line!(
        "Mid Price:",
        format!("${:<12.2}", ob.mid_price).bright_white()
    );
    print_line!(
        "Spread:",
        format!(
            "${:<12.2} ({:>5.2} bps)",
            (ob.best_ask.0 - ob.best_bid.0).normalize(),
            ob.spread_bps
        )
    );

    println!(
        "{}",
        border_color("╠══════════════════════════════════════════════════════╣")
    );
    println!("║  Depth (within 10 bps):                              ║");
    print_line!(
        "  Bids:",
        format!(
            "{:<10.4} {} (${:<12.2})",
            bid_depth_10, base_asset, bid_cost_10
        )
    );
    print_line!(
        "  Asks:",
        format!(
            "{:<10.4} {} (${:<12.2})",
            ask_depth_10, base_asset, ask_cost_10
        )
    );

    let imb_label = if imb > Decimal::ZERO {
        "buy pressure".green()
    } else {
        "sell pressure".red()
    };
    print_line!("Imbalance:", format!("{:<5.2} ({})", imb, imb_label));

    println!(
        "{}",
        border_color("╠══════════════════════════════════════════════════════╣")
    );
    println!(
        "║  Walk-the-book (2 {} buy):                         ║",
        base_asset
    );
    print_line!("  Avg price:", format!("${:<12.2}", walk_2.avg_price));
    print_line!("  Slippage:", format!("{:<5.2} bps", walk_2.slippage_bps));

    println!(
        "║  Walk-the-book (10 {} buy):                        ║",
        base_asset
    );
    print_line!("  Avg price:", format!("${:<12.2}", walk_10.avg_price));
    print_line!("  Slippage:", format!("{:<5.2} bps", walk_10.slippage_bps));
    print_line!("  Levels:", format!("{:<5}", walk_10.levels_consumed));

    println!(
        "{}",
        border_color("╠══════════════════════════════════════════════════════╣")
    );
    println!(
        "║  Effective spread (2 {} round-trip): {:>6} bps    ║",
        base_asset,
        format!("{:.2}", eff_spread).yellow()
    );
    println!(
        "{}",
        border_color("╚══════════════════════════════════════════════════════╝")
    );

    Ok(())
}
