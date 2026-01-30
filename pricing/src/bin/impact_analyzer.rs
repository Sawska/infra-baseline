#![allow(clippy::collapsible_if)]
use alloy_primitives::U256;
use anyhow::{Context, Result};
use arb_chain::ChainClient;
use arb_core::Address;
use clap::Parser;
use dotenvy::dotenv;
use pricing::amm::{Pool, Token};
use rust_decimal::prelude::*;
use serde::Serialize;
use std::env;

#[derive(Parser, Debug)]
#[command(author, version, about = "Price Impact Analyzer")]
struct Args {
    /// Pair address
    pair_address: String,

    /// Token In Symbol or Address (e.g., "USDC" or "0xA0b8...")
    #[arg(long)]
    token_in: String,

    /// Comma separated list of amounts (in human readable format, e.g. 1000,2000)
    #[arg(long, value_delimiter = ',')]
    sizes: Vec<String>,

    /// RPC URL
    #[arg(long)]
    rpc: Option<String>,

    /// Start block for historical analysis (optional)
    #[arg(long)]
    start_block: Option<u64>,

    /// End block for historical analysis (optional)
    #[arg(long)]
    end_block: Option<u64>,

    /// Block step size for historical analysis
    #[arg(long, default_value_t = 100)]
    step: u64,
}

#[derive(Debug, Serialize)]
pub struct ImpactResult {
    pub amount_in: U256,
    pub amount_out: U256,
    pub spot_price: Decimal,
    pub execution_price: Decimal,
    pub price_impact_pct: Decimal,
    pub block: Option<u64>,
}

pub struct PriceImpactAnalyzer {
    pub pair: Pool,
}

impl PriceImpactAnalyzer {
    pub fn new(pair: Pool) -> Self {
        Self { pair }
    }

    pub fn generate_impact_table(
        &self,
        token_in: &Token,
        sizes: Vec<U256>,
        block: Option<u64>,
    ) -> Result<Vec<ImpactResult>> {
        let spot_price = self.pair.get_spot_price(token_in)?;
        let mut results = Vec::new();

        for amount_in in sizes {
            let amount_out = self.pair.get_amount_out(amount_in, token_in)?;
            let execution_price = self.pair.get_execution_price(amount_in, token_in)?;
            let price_impact = self.pair.get_price_impact(amount_in, token_in)?;

            results.push(ImpactResult {
                amount_in,
                amount_out,
                spot_price,
                execution_price,
                price_impact_pct: price_impact * Decimal::from(100),
                block,
            });
        }

        Ok(results)
    }

    pub fn find_max_size_for_impact(
        &self,
        token_in: &Token,
        max_impact_pct: Decimal,
    ) -> Result<U256> {
        let mut low = U256::from(1);
        let mut high = U256::from(10).pow(U256::from(30));
        if let Pool::V2(ref p) = self.pair {
            if let Ok((res, _)) = p.get_reserves_ordered(token_in) {
                high = res;
            }
        }
        let mut best = U256::ZERO;
        let target_decimal = max_impact_pct / Decimal::from(100);

        for _ in 0..128 {
            if low > high {
                break;
            }
            let mid = (low + high) / U256::from(2);
            if mid.is_zero() {
                low = U256::from(1);
                continue;
            }

            let impact_res = self.pair.get_price_impact(mid, token_in);

            match impact_res {
                Ok(impact) => {
                    if impact <= target_decimal {
                        best = mid;
                        low = mid + U256::from(1);
                    } else {
                        high = mid - U256::from(1);
                    }
                }
                Err(_) => {
                    high = mid - U256::from(1);
                }
            }
        }

        Ok(best)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let args = Args::parse();

    let rpc_url = args
        .rpc
        .or_else(|| env::var("RPC_URL").ok())
        .context("RPC URL is required (env or flag)")?;

    println!("Connecting to {}...", rpc_url);
    let client = ChainClient::new(&rpc_url);

    let pair_addr = Address::from_string(&args.pair_address)?;

    let start = args.start_block;
    let end = args.end_block;

    let blocks_to_analyze: Vec<Option<u64>> = if let (Some(s), Some(e)) = (start, end) {
        (s..=e).step_by(args.step as usize).map(Some).collect()
    } else {
        vec![None]
    };

    println!(
        "Starting analysis for {} snapshots...",
        blocks_to_analyze.len()
    );

    let initial_pool = Pool::from_chain(pair_addr, &client, None).await?;
    let (t0, t1) = initial_pool.tokens();
    let token_in_meta = if args.token_in.eq_ignore_ascii_case(&t0.symbol)
        || args.token_in.eq_ignore_ascii_case(&t0.address.to_string())
    {
        t0.clone()
    } else if args.token_in.eq_ignore_ascii_case(&t1.symbol)
        || args.token_in.eq_ignore_ascii_case(&t1.address.to_string())
    {
        t1.clone()
    } else {
        anyhow::bail!("Token In '{}' not found in pair", args.token_in);
    };
    let token_out_meta = if token_in_meta.address == t0.address {
        t1.clone()
    } else {
        t0.clone()
    };

    let decimals = token_in_meta.decimals;
    let multiplier = U256::from(10).pow(U256::from(decimals));
    let mut raw_sizes = Vec::new();
    for s in &args.sizes {
        let val_dec = Decimal::from_str(s).context("Invalid size format")?;
        let raw_val = U256::from_str_radix(&val_dec.trunc().to_string(), 10).unwrap_or(U256::ZERO)
            * multiplier;
        raw_sizes.push(raw_val);
    }

    println!(
        "\n┌{:─<10}┬{:─<10}┬{:─<15}┬{:─<10}┬{:─<10}┐",
        "", "", "", "", ""
    );
    println!(
        "│ {:<8} │ {:<8} │ {:<13} │ {:<8} │ {:<8} │",
        "Block", "In", "Out", "Price", "Impact"
    );
    println!(
        "├{:─<10}┼{:─<10}┼{:─<15}┼{:─<10}┼{:─<10}┤",
        "", "", "", "", ""
    );

    for block_opt in blocks_to_analyze {
        let block_label = block_opt
            .map(|b| b.to_string())
            .unwrap_or_else(|| "Latest".to_string());

        let pool = match Pool::from_chain(pair_addr, &client, block_opt).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to fetch pool at block {}: {}", block_label, e);
                continue;
            }
        };

        let analyzer = PriceImpactAnalyzer::new(pool);
        match analyzer.generate_impact_table(&token_in_meta, raw_sizes.clone(), block_opt) {
            Ok(table) => {
                for row in table {
                    let amount_in_human = to_human(row.amount_in, decimals);
                    let amount_out_human = to_human(row.amount_out, token_out_meta.decimals);

                    println!(
                        "│ {:<8} │ {:<8} │ {:<13} │ {:<8} │ {:<8} │",
                        block_label,
                        format!("{:.0}", amount_in_human),
                        format!("{:.6}", amount_out_human),
                        format!("{:.6}", row.execution_price),
                        format!("{:.2}%", row.price_impact_pct)
                    );
                }
            }
            Err(e) => eprintln!("Error calculating impact at block {}: {}", block_label, e),
        }
    }
    println!(
        "└{:─<10}┴{:─<10}┴{:─<15}┴{:─<10}┴{:─<10}┘",
        "", "", "", "", ""
    );

    Ok(())
}

fn to_human(val: U256, decimals: u8) -> Decimal {
    let s = val.to_string();
    let d = Decimal::from_str(&s).unwrap_or(Decimal::ZERO);
    let m = Decimal::from(10u64.pow(decimals as u32));
    d / m
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_core::Address;
    use pricing::amm::UniswapV2Pair;

    fn make_addr(byte: u8) -> Address {
        let mut bytes = [0u8; 20];
        bytes[19] = byte;
        Address(alloy_primitives::Address::from(bytes))
    }

    #[test]
    fn test_analyzer_binary_search() {
        let t0 = Token::new(make_addr(1), 18, "ETH".to_string());
        let t1 = Token::new(make_addr(2), 6, "USDC".to_string());

        let r0 = U256::from(100_000) * U256::from(10).pow(U256::from(18));
        let r1 = U256::from(100_000_000) * U256::from(10).pow(U256::from(6));

        let pair = UniswapV2Pair::new(make_addr(3), t0.clone(), t1.clone(), r0, r1, 30);
        let analyzer = PriceImpactAnalyzer::new(pricing::amm::Pool::V2(pair));

        let target_impact = Decimal::from(1);
        let max_size = analyzer
            .find_max_size_for_impact(&t1, target_impact)
            .unwrap();

        let actual_impact = analyzer.pair.get_price_impact(max_size, &t1).unwrap();
        let actual_pct = actual_impact * Decimal::from(100);

        assert!(actual_pct <= target_impact);
        assert!(actual_pct > Decimal::from_str("0.99").unwrap());
    }

    #[test]
    fn test_table_generation() {
        let t0 = Token::new(make_addr(1), 18, "ETH".to_string());
        let t1 = Token::new(make_addr(2), 6, "USDC".to_string());
        let r0 = U256::from(1000) * U256::from(10).pow(U256::from(18));
        let r1 = U256::from(2_000_000) * U256::from(10).pow(U256::from(6));

        let pair = UniswapV2Pair::new(make_addr(3), t0.clone(), t1.clone(), r0, r1, 30);
        let analyzer = PriceImpactAnalyzer::new(pricing::amm::Pool::V2(pair));

        let sizes = vec![
            U256::from(1000) * U256::from(10).pow(U256::from(6)),
            U256::from(10000) * U256::from(10).pow(U256::from(6)),
        ];

        let table = analyzer.generate_impact_table(&t1, sizes, None).unwrap();

        assert_eq!(table.len(), 2);
        assert!(table[0].price_impact_pct < table[1].price_impact_pct);
    }

    #[test]
    fn test_historical_block_propagation() {
        let t0 = Token::new(make_addr(1), 18, "ETH".to_string());
        let t1 = Token::new(make_addr(2), 6, "USDC".to_string());
        let r0 = U256::from(1000) * U256::from(10).pow(U256::from(18));
        let r1 = U256::from(2_000_000) * U256::from(10).pow(U256::from(6));

        let pair = UniswapV2Pair::new(make_addr(3), t0.clone(), t1.clone(), r0, r1, 30);
        let analyzer = PriceImpactAnalyzer::new(pricing::amm::Pool::V2(pair));

        let sizes = vec![U256::from(100)];

        let target_block = 15_000_000;
        let table_history = analyzer
            .generate_impact_table(&t1, sizes.clone(), Some(target_block))
            .unwrap();

        assert_eq!(table_history.len(), 1);
        assert_eq!(
            table_history[0].block,
            Some(target_block),
            "The specific block number should be attached to the result"
        );

        let table_latest = analyzer.generate_impact_table(&t1, sizes, None).unwrap();

        assert_eq!(table_latest.len(), 1);
        assert_eq!(
            table_latest[0].block, None,
            "None should be propagated when analyzing latest state"
        );
    }
}
