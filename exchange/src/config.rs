use crate::client::ExchangeType;
use anyhow::{Context, Result};
use std::env;

#[derive(Debug, Clone)]
pub struct ExchangeConfig {
    pub production: bool,

    pub binance_http_url: String,
    pub binance_ws_url: String,
    pub cex_fee_bps: f64,

    pub arbitrum_rpc_url: String,
    pub arbitrum_chain_id: u64,

    pub pair: String,
    pub weth_address: String,
    pub usdc_address: String,

    pub api_key: String,
    pub secret: String,
    pub is_sandbox: bool,
    pub skip_connection_validation: bool,
}

impl ExchangeConfig {
    pub fn from_env(exchange_type: ExchangeType) -> Result<Self> {
        dotenvy::dotenv().ok();

        let production = env::var("PRODUCTION")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase()
            == "true";

        let (binance_http_url, binance_ws_url, cex_fee_bps) = if production {
            (
                "https://api.binance.com".to_string(),
                "wss://stream.binance.com:9443/ws".to_string(),
                10.0,
            )
        } else {
            (
                "https://testnet.binance.vision".to_string(),
                "wss://testnet.binance.vision/ws".to_string(),
                0.0,
            )
        };

        let arbitrum_rpc_url = env::var("ARBITRUM_RPC_URL")
            .unwrap_or_else(|_| "https://arb1.arbitrum.io/rpc".to_string());

        let arbitrum_chain_id = 42161;

        let pair = "ETH/USDC".to_string();

        let weth_address = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".to_string();
        let usdc_address = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string();

        let (key_name, secret_name) = match (exchange_type, production) {
            (ExchangeType::Binance, true) => ("BINANCE_API_KEY", "BINANCE_SECRET"),
            (ExchangeType::Binance, false) => ("BINANCE_TESTNET_API_KEY", "BINANCE_TESTNET_SECRET"),
            (ExchangeType::Bybit, true) => ("BYBIT_API_KEY", "BYBIT_SECRET"),
            (ExchangeType::Bybit, false) => ("BYBIT_TESTNET_API_KEY", "BYBIT_TESTNET_SECRET"),
        };

        let api_key = env::var(key_name).with_context(|| {
            format!(
                "Variable {} not found in .env (Production={})",
                key_name, production
            )
        })?;

        let secret = env::var(secret_name).with_context(|| {
            format!(
                "Variable {} not found in .env (Production={})",
                secret_name, production
            )
        })?;

        Ok(Self {
            production,
            binance_http_url,
            binance_ws_url,
            cex_fee_bps,
            arbitrum_rpc_url,
            arbitrum_chain_id,
            pair,
            weth_address,
            usdc_address,
            api_key,
            secret,
            is_sandbox: false,
            skip_connection_validation: false,
        })
    }
}
