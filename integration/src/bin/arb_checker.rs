use anyhow::{Context, Result};
use arb_chain::ChainClient;
use arb_core::Address;
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use integration::checker::{ArbChecker, ArbOpportunity};
use inventory::pnl::PnLEngine;
use inventory::tracker::{InventoryTracker, Venue};
use pricing::amm::Token;
use pricing::engine::PricingEngine;
use rust_decimal_macros::dec;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;

const WETH_ADDR: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
const USDT_ADDR: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

fn log_opportunity_to_csv(opp: &ArbOpportunity, filepath: &str) -> Result<()> {
    let file_exists = std::path::Path::new(filepath).exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(filepath)?;

    if !file_exists {
        writeln!(
            file,
            "timestamp,pair,dex_price,cex_bid,gap_bps,costs_bps,net_pnl_bps,inventory_ok,executable"
        )?;
    }

    writeln!(
        file,
        "{},{},{},{},{},{},{},{},{}",
        opp.timestamp.to_rfc3339(),
        opp.pair,
        opp.dex_price,
        opp.cex_bid,
        opp.gap_bps,
        opp.estimated_costs_bps,
        opp.estimated_net_pnl_bps,
        opp.inventory_ok,
        opp.executable
    )?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let rpc_url = env::var("RPC_URL_LOCAL").context("RPC_URL not found in environment")?;
    let wss_url = env::var("WS_URL").context("WSS_URL not found in environment")?;

    let client = Arc::new(ChainClient::new(&rpc_url));

    let mut pricing_engine = PricingEngine::new(client.clone(), &rpc_url, &wss_url, |_| async {})?;

    let pools = vec![
        Address::from_string("0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852").unwrap(),
        Address::from_string("0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc").unwrap(),
        Address::from_string("0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11").unwrap(),
    ];
    pricing_engine.load_pools(pools).await?;

    let ex_config = ExchangeConfig::from_env(ExchangeType::Bybit)?;
    let exchange_client = ExchangeClient::new(ex_config, ExchangeType::Bybit)
        .await
        .expect("Failed to create exchange client");

    let mut tracker = InventoryTracker::new(None);
    tracker.update_from_cex(Venue::Cex, {
        let mut m = std::collections::HashMap::new();
        m.insert("ETH".to_string(), (dec!(8.0), dec!(0.0)));
        m.insert("USDT".to_string(), (dec!(15000.0), dec!(0.0)));
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

    let size = dec!(2.0);
    println!("═══════════════════════════════════════════");
    println!("  ARB CHECK: ETH/USDT (size: {} ETH)", size);
    println!("═══════════════════════════════════════════");

    match checker.check("ETH/USDT", size, &eth, &usdt).await {
        Ok(opp) => {
            println!("\nPrices:");
            println!(
                "  Uniswap V2:      ${:.2} (buy {} ETH)",
                opp.dex_price, size
            );
            println!("  Binance bid:      ${:.2}", opp.cex_bid);

            let gap_usd = opp.cex_bid - opp.dex_price;
            println!("\nGap: ${:.2} ({:.1} bps)", gap_usd, opp.gap_bps);

            println!("\nCosts:");
            println!("  DEX fee:           {:.1} bps", opp.details.dex_fee_bps);
            println!(
                "  DEX price impact:   {:.1} bps",
                opp.details.dex_price_impact_bps
            );
            println!("  CEX fee:           {:.1} bps", opp.details.cex_fee_bps);
            println!(
                "  CEX slippage:       {:.1} bps",
                opp.details.cex_slippage_bps
            );
            let gas_bps = (opp.details.gas_cost_usd / (size * opp.dex_price)) * dec!(10000);
            println!(
                "  Gas:               ${:.2} ({:.1} bps)",
                opp.details.gas_cost_usd, gas_bps
            );
            println!("  ────────────────────────");
            println!("  Total costs:       {:.1} bps", opp.estimated_costs_bps);

            let pnl_icon = if opp.estimated_net_pnl_bps > dec!(0) {
                "✅"
            } else {
                "❌ NOT PROFITABLE"
            };
            println!(
                "\nNet PnL estimate: {:.1} bps {}",
                opp.estimated_net_pnl_bps, pnl_icon
            );

            println!("\nInventory:");
            let wallet_usdt = checker
                .inventory_tracker
                .get_available(Venue::Wallet, "USDT");
            let binance_eth = checker.inventory_tracker.get_available(Venue::Cex, "ETH");
            println!(
                "  Wallet USDT:  {:<6} (need ~{:.0}) ✅",
                wallet_usdt,
                size * opp.dex_price
            );
            println!(
                "  Binance ETH:   {:<6} (need {:.1})    ✅",
                binance_eth, size
            );

            let verdict = if opp.executable {
                "PROCEED"
            } else {
                "SKIP — costs exceed gap"
            };
            println!("\nVerdict: {}", verdict);

            if let Err(e) = log_opportunity_to_csv(&opp, "arb_opportunities.csv") {
                eprintln!("Warning: Failed to log opportunity to CSV: {}", e);
            } else {
                println!("\n📝 Result logged to arb_opportunities.csv");
            }
        }
        Err(e) => println!("Error during check: {:?}", e),
    }
    println!("═══════════════════════════════════════════");

    Ok(())
}
