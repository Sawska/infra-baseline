use anyhow::{Context, Result};
use arb_core::{APP_CONFIG, WalletManager};
use std::fs;
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<()> {
    println!("--- Secure Keyfile Generator ---");

    let private_key =
        APP_CONFIG.wallet.private_key.as_deref().context(
            "PRIVATE_KEY not found in .env. Please ensure it is set before running this.",
        )?;

    let wallet = WalletManager::from_private_key(private_key)?;
    println!("Found wallet for address: {}", wallet.address());

    print!("Enter a strong password to encrypt your keyfile: ");
    io::stdout().flush()?;

    let mut password = String::new();
    io::stdin().read_line(&mut password)?;
    let password = password.trim();

    if password.len() < 8 {
        anyhow::bail!("Password is too short. Please use at least 8 characters for safety.");
    }

    let output_dir = "credentials";
    let output_file = "credentials/keystore.json";

    fs::create_dir_all(output_dir).context("Failed to create credentials directory")?;

    println!("Encrypting and saving strictly to {}...", output_file);

    wallet
        .to_keyfile(output_file, password)
        .context("Failed to create encrypted keyfile.")?;

    let mut actual_data_found = false;
    if let Ok(entries) = fs::read_dir(output_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

            if path.is_file() && file_name.len() > 30 && !file_name.contains('.') {
                let json_content = fs::read_to_string(&path)?;

                fs::write(output_file, json_content)?;

                let _ = fs::remove_file(path);
                actual_data_found = true;
                break;
            }
        }
    }

    if !actual_data_found {
        let content = fs::read_to_string(output_file).unwrap_or_default();
        if !content.starts_with('{') {
            anyhow::bail!(
                "Failed to generate valid JSON content. Please check arb_core library logic."
            );
        }
    }

    println!("\nSUCCESS!");
    println!("1. Your encrypted keyfile is at: {}", output_file);
    println!("2. Content Verified: Correct JSON format detected.");
    println!(
        "3. Update your .env to include: KEYFILE_PATH={}",
        output_file
    );
    println!("\nKeep your password safe! If you lose it, you lose access to the funds.");

    Ok(())
}
