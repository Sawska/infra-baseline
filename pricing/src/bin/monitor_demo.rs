use anyhow::{Context, Result};
use clap::Parser;
use dotenvy::dotenv;
use pricing::monitor::{MempoolMonitor, MonitorEvent};
use rustls::crypto::ring;
use std::env;

#[derive(Parser, Debug)]
#[command(author, version, about = "Mempool Monitor Demo")]
struct Args {
    #[arg(long)]
    ws: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = ring::default_provider().install_default();
    dotenv().ok();
    let args = Args::parse();

    let ws_url = args
        .ws
        .or_else(|| env::var("WS_URL").ok())
        .context("WS URL is required")?;

    println!("========================================");
    println!("      MEMPOOL MONITOR ACTIVATED         ");
    println!("========================================");
    println!("Connecting to: {}", ws_url);
    println!("Listening for:");
    println!(" - Pending Swaps (Uniswap V2 & V3)");
    println!(" - Live Log Events (Sync & Swap)");
    println!("----------------------------------------\n");

    let monitor = MempoolMonitor::new(ws_url, |event| async move {
        match event {
            MonitorEvent::MempoolSwap(swap) => {
                println!("[PENDING TX] Hash: {:?}", swap.tx_hash);
                println!("   Protocol: {} ({})", swap.dex, swap.method);
                println!("   Input:    {}", swap.amount_in);
                if let Some(t_in) = swap.token_in {
                    println!("   Token In: {:?}", t_in);
                }
                println!("----------------------------------------");
            }
            MonitorEvent::PoolUpdate {
                pool_address,
                block_number,
            } => {
                println!("[EVENT LOG]  Block: {}", block_number);
                println!("   Pool:     {:?}", pool_address);
                println!("   Action:   Reserve Update / Swap");
                println!("----------------------------------------");
            }
        }
    });

    monitor.start().await?;

    Ok(())
}
