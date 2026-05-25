use anyhow::{Context, Result};
use arb_chain::{ChainClient, TransactionBuilder};
use arb_core::{APP_CONFIG, Address, TokenAmount, WalletManager};
use std::io::{self, Write};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let keyfile_raw = APP_CONFIG
        .wallet
        .keyfile_path
        .clone()
        .context("KEYFILE_PATH env var must be set in .env")?;

    let cleaned_path = keyfile_raw.trim().trim_matches('"').trim_matches('\'');
    let keyfile_path = PathBuf::from(cleaned_path);

    if !keyfile_path.exists() {
        anyhow::bail!(
            "Keyfile not found at: {:?}\n(Check if the path in .env matches the filename in your credentials/ folder)",
            keyfile_path
        );
    }

    print!("Enter password for wallet at {:?}: ", keyfile_path);
    io::stdout().flush()?;

    let mut password = String::new();
    io::stdin().read_line(&mut password)?;
    let password = password.trim();

    let wallet = WalletManager::from_keyfile(&keyfile_path, password)
        .context("Failed to unlock wallet. Check your password and file integrity.")?;

    let rpc_url = APP_CONFIG
        .chain
        .rpc_url
        .clone()
        .context("RPC_URL env var must be set in .env")?;
    let client = ChainClient::new(&rpc_url);

    let chain_id = APP_CONFIG.chain.chain_id;

    println!("\nAuthenticated Wallet: {}", wallet.address());
    let balance = client.get_balance(wallet.address()).await?;
    println!(
        "Balance: {} {}",
        balance.to_human(),
        balance.symbol.unwrap_or_default()
    );

    let recipient = Address::from_string("0x70997970C51812dc3A010C7d01b50e0d17dc79C8")?;
    let amount = TokenAmount::from_human("0.001", 18, Some("ETH".to_string()))?;

    println!("\nBuilding transaction...");
    println!("  To: {}", recipient);
    println!("  Value: {} ETH", amount.to_human());

    let builder = TransactionBuilder::new(&client, &wallet)
        .to(recipient)
        .value(amount)
        .with_gas_estimate(1.1)
        .chain_id(chain_id)
        .with_gas_price("fast");

    let signed_tx_bytes = builder.build_and_sign().await?;
    println!("  Signed tx (RLP hex): 0x{}", hex::encode(&signed_tx_bytes));

    println!("\nSending transaction...");
    let tx_hash = client.send_transaction(signed_tx_bytes).await?;
    println!("  TX Hash: {:?}", tx_hash);

    println!("\nWaiting for confirmation...");
    let receipt = client.wait_for_receipt(tx_hash).await?;

    println!("  Confirmed in Block: {}", receipt.block_number);
    println!(
        "  Status: {}",
        if receipt.status {
            "✅ SUCCESS"
        } else {
            "❌ FAILED"
        }
    );
    println!("  Gas Used: {}", receipt.gas_used);

    let fee = receipt.tx_fee();
    println!("  Fee Paid: {} ETH", fee.to_human());

    if !receipt.status {
        anyhow::bail!("Transaction failed on-chain");
    }

    println!("\nLifecycle Script COMPLETED");
    Ok(())
}
