use alloy_primitives::U256;
use anyhow::{Context, Result};
use arb_chain::ChainClient;
use arb_core::{APP_CONFIG, Address};
use clap::Parser;
use pricing::{amm::Pool, amm::Token, router::RouteFinder};
use rust_decimal::prelude::*;

#[derive(Parser, Debug)]
#[command(author, version, about = "Router Demo")]
struct Args {
    #[arg(long, value_delimiter = ',', required = true)]
    pools: Vec<String>,

    #[arg(long)]
    token_in: String,

    #[arg(long)]
    token_out: String,

    #[arg(long)]
    amount: String,

    #[arg(long)]
    rpc: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let rpc_url = args
        .rpc
        .or_else(|| APP_CONFIG.chain.rpc_url_local.clone())
        .context("RPC URL is required")?;
    let client = ChainClient::new(&rpc_url);

    println!("1. Loading Pools...");
    let mut pools = Vec::new();
    for addr_str in args.pools {
        let addr = Address::from_string(&addr_str)?;
        match Pool::from_chain(addr, &client, None).await {
            Ok(p) => {
                let (t0, t1) = p.tokens();
                println!("   Loaded: {} ({}/{})", addr, t0.symbol, t1.symbol);
                pools.push(p);
            }
            Err(e) => println!("   Failed to load {}: {}", addr, e),
        }
    }

    let finder = RouteFinder::new(pools.clone());

    let all_tokens: Vec<Token> = pools
        .iter()
        .flat_map(|p| {
            let (t0, t1) = p.tokens();
            vec![t0, t1]
        })
        .collect();

    let token_in = all_tokens
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case(&args.token_in))
        .context(format!("Token {} not found in loaded pools", args.token_in))?;

    let token_out = all_tokens
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case(&args.token_out))
        .context(format!(
            "Token {} not found in loaded pools",
            args.token_out
        ))?;

    let amount_dec = Decimal::from_str(&args.amount)?;
    let amount_raw = U256::from_str_radix(&amount_dec.trunc().to_string(), 10)
        .unwrap_or(U256::ZERO)
        * U256::from(10).pow(U256::from(token_in.decimals));

    println!(
        "\n2. Finding Best Route: {} {} -> {}",
        args.amount, token_in.symbol, token_out.symbol
    );

    match finder.find_best_route(token_in, token_out, amount_raw, 30, 3) {
        Ok((route, output)) => {
            println!("\n✅ ROUTE FOUND");
            println!(
                "Path: {}",
                route
                    .path
                    .iter()
                    .map(|t| t.symbol.clone())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            );

            let output_human = to_human(output, token_out.decimals);
            println!("Estimated Output: {:.4} {}", output_human, token_out.symbol);

            let gas = route.estimate_gas();
            println!("Est. Gas Used: {}", gas);
        }
        Err(e) => {
            println!("\n❌ No route found: {}", e);
        }
    }

    Ok(())
}

fn to_human(val: U256, decimals: u8) -> Decimal {
    let s = val.to_string();
    let d = Decimal::from_str(&s).unwrap_or(Decimal::ZERO);
    let m = Decimal::from(10u64.pow(decimals as u32));
    d / m
}
