use crate::client::ExchangeType;
use anyhow::{Context, Result};
use std::env;

pub struct ExchangeConfig {
    pub api_key: String,
    pub secret: String,
    pub is_sandbox: bool,
    pub skip_connection_validation: bool,
}

impl ExchangeConfig {
    pub fn from_env(exchange: ExchangeType) -> Result<Self> {
        dotenvy::dotenv().ok();

        let (key_name, secret_name) = match exchange {
            ExchangeType::Binance => ("BINANCE_TESTNET_API_KEY", "BINANCE_TESTNET_SECRET"),
            ExchangeType::Bybit => ("BYBIT_TESTNET_API_KEY", "BYBIT_TESTNET_SECRET"),
        };

        let is_sandbox = env::var("IS_SANDBOX").unwrap_or_else(|_| "true".to_string()) == "true";

        Ok(Self {
            api_key: env::var(key_name)
                .with_context(|| format!("Variable {} not found in .env", key_name))?,
            secret: env::var(secret_name)
                .with_context(|| format!("Variable {} not found in .env", secret_name))?,
            is_sandbox,
            skip_connection_validation: false,
        })
    }
}
