use crate::client::ExchangeType;
use anyhow::{Context, Result};
use arb_core::config::{APP_CONFIG, ExchangeVenueConfig};

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
    pub passphrase: Option<String>,
    pub is_sandbox: bool,
    pub skip_connection_validation: bool,
}

impl ExchangeConfig {
    pub fn from_env(exchange_type: ExchangeType) -> Result<Self> {
        let app = &*APP_CONFIG;
        let production = app.runtime.production;
        let (venue, key_name, secret_name, passphrase_name) = venue_for(exchange_type, production);

        let api_key = venue.api_key.clone_value().with_context(|| {
            format!(
                "Variable {} not found in .env (Production={})",
                key_name, production
            )
        })?;

        let secret = venue.secret.clone_value().with_context(|| {
            format!(
                "Variable {} not found in .env (Production={})",
                secret_name, production
            )
        })?;

        let passphrase = passphrase_name
            .map(|name| {
                venue.passphrase.clone_value().with_context(|| {
                    format!(
                        "Variable {} not found in .env (Production={})",
                        name, production
                    )
                })
            })
            .transpose()?;

        Ok(Self {
            production,
            binance_http_url: venue.http_url.clone(),
            binance_ws_url: venue.ws_url.clone(),
            cex_fee_bps: app.exchange.fee_bps(production),
            arbitrum_rpc_url: app.chain.arbitrum_rpc_url.clone(),
            arbitrum_chain_id: app.chain.arbitrum_chain_id,
            pair: app.exchange.default_pair.clone(),
            weth_address: app.tokens.arbitrum_weth.clone(),
            usdc_address: app.tokens.arbitrum_usdc.clone(),
            api_key,
            secret,
            passphrase,
            is_sandbox: app.exchange.is_sandbox || !production,
            skip_connection_validation: app.exchange.skip_connection_validation,
        })
    }
}

fn venue_for(
    exchange_type: ExchangeType,
    production: bool,
) -> (
    &'static ExchangeVenueConfig,
    &'static str,
    &'static str,
    Option<&'static str>,
) {
    let app = &*APP_CONFIG;
    match (exchange_type, production) {
        (ExchangeType::Binance, true) => (
            &app.exchange.binance,
            "BINANCE_API_KEY",
            "BINANCE_SECRET",
            None,
        ),
        (ExchangeType::Binance, false) => (
            &app.exchange.binance_testnet,
            "BINANCE_TESTNET_API_KEY",
            "BINANCE_TESTNET_SECRET",
            None,
        ),
        (ExchangeType::Bybit, true) => (&app.exchange.bybit, "BYBIT_API_KEY", "BYBIT_SECRET", None),
        (ExchangeType::Bybit, false) => (
            &app.exchange.bybit_testnet,
            "BYBIT_TESTNET_API_KEY",
            "BYBIT_TESTNET_SECRET",
            None,
        ),
        (ExchangeType::Okx, _) => (
            &app.exchange.okx,
            "OKX_API_KEY",
            "OKX_SECRET",
            Some("OKX_PASSPHRASE"),
        ),
        (ExchangeType::Coinbase, _) => (
            &app.exchange.coinbase,
            "COINBASE_API_KEY",
            "COINBASE_SECRET",
            None,
        ),
        (ExchangeType::Kraken, _) => (
            &app.exchange.kraken,
            "KRAKEN_API_KEY",
            "KRAKEN_SECRET",
            None,
        ),
    }
}
