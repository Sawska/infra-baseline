use once_cell::sync::Lazy;
use std::{env, fmt, str::FromStr};

pub static APP_CONFIG: Lazy<AppConfig> = Lazy::new(AppConfig::new);

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub runtime: RuntimeConfig,
    pub chain: ChainConfig,
    pub wallet: WalletConfig,
    pub exchange: ExchangeSettings,
    pub tokens: TokenConfig,
    pub dex: DexConfig,
    pub telegram: TelegramConfig,
    pub flashbots: FlashbotsConfig,
}

impl AppConfig {
    pub fn new() -> Self {
        dotenvy::dotenv().ok();

        Self {
            runtime: RuntimeConfig::new(),
            chain: ChainConfig::new(),
            wallet: WalletConfig::new(),
            exchange: ExchangeSettings::new(),
            tokens: TokenConfig::new(),
            dex: DexConfig::new(),
            telegram: TelegramConfig::new(),
            flashbots: FlashbotsConfig::new(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub production: bool,
    pub simulation: bool,
    pub dry_run: bool,
    pub database_url: String,
}

impl RuntimeConfig {
    pub fn new() -> Self {
        Self {
            production: env_bool("PRODUCTION", false),
            simulation: env_bool("SIMULATION", false),
            dry_run: env_bool("DRY_RUN", false),
            database_url: env_string("DATABASE_URL", ""),
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub rpc_url: Option<String>,
    pub rpc_url_local: Option<String>,
    pub ws_url: Option<String>,
    pub eth_rpc_url: Option<String>,
    pub arbitrum_rpc_url: String,
    pub chain_id: u64,
    pub ethereum_chain_id: u64,
    pub arbitrum_chain_id: u64,
}

impl ChainConfig {
    pub fn new() -> Self {
        Self {
            rpc_url: env_optional("RPC_URL"),
            rpc_url_local: env_optional("RPC_URL_LOCAL"),
            ws_url: env_optional("WS_URL"),
            eth_rpc_url: env_optional("ETH_RPC_URL"),
            arbitrum_rpc_url: env_string("ARBITRUM_RPC_URL", "https://arb1.arbitrum.io/rpc"),
            chain_id: env_parse("CHAIN_ID", 11_155_111),
            ethereum_chain_id: env_parse("ETHEREUM_CHAIN_ID", 1),
            arbitrum_chain_id: env_parse("ARBITRUM_CHAIN_ID", 42_161),
        }
    }

    pub fn rpc_url_or_local(&self) -> Option<&str> {
        self.rpc_url.as_deref().or(self.rpc_url_local.as_deref())
    }
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct WalletConfig {
    pub private_key: SecretValue,
    pub keyfile_path: Option<String>,
}

impl WalletConfig {
    pub fn new() -> Self {
        Self {
            private_key: SecretValue::from_env("PRIVATE_KEY"),
            keyfile_path: env_optional("KEYFILE_PATH"),
        }
    }
}

impl Default for WalletConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for WalletConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalletConfig")
            .field("private_key", &self.private_key)
            .field("keyfile_path", &self.keyfile_path)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ExchangeSettings {
    pub binance: ExchangeVenueConfig,
    pub binance_testnet: ExchangeVenueConfig,
    pub bybit: ExchangeVenueConfig,
    pub bybit_testnet: ExchangeVenueConfig,
    pub okx: ExchangeVenueConfig,
    pub coinbase: ExchangeVenueConfig,
    pub kraken: ExchangeVenueConfig,
    pub production_fee_bps: f64,
    pub testnet_fee_bps: f64,
    pub default_pair: String,
    pub is_sandbox: bool,
    pub skip_connection_validation: bool,
}

impl ExchangeSettings {
    pub fn new() -> Self {
        Self {
            binance: ExchangeVenueConfig::new(
                "BINANCE_HTTP_URL",
                "https://api.binance.com",
                "BINANCE_WS_URL",
                "wss://stream.binance.com:9443/ws",
                "BINANCE_API_KEY",
                "BINANCE_SECRET",
                None,
            ),
            binance_testnet: ExchangeVenueConfig::new(
                "BINANCE_TESTNET_HTTP_URL",
                "https://testnet.binance.vision",
                "BINANCE_TESTNET_WS_URL",
                "wss://testnet.binance.vision/ws",
                "BINANCE_TESTNET_API_KEY",
                "BINANCE_TESTNET_SECRET",
                None,
            ),
            bybit: ExchangeVenueConfig::new(
                "BYBIT_HTTP_URL",
                "https://api.bybit.com",
                "BYBIT_WS_URL",
                "wss://stream.bybit.com/v5/public/spot",
                "BYBIT_API_KEY",
                "BYBIT_SECRET",
                None,
            ),
            bybit_testnet: ExchangeVenueConfig::new(
                "BYBIT_TESTNET_HTTP_URL",
                "https://api-testnet.bybit.com",
                "BYBIT_TESTNET_WS_URL",
                "wss://stream-testnet.bybit.com/v5/public/spot",
                "BYBIT_TESTNET_API_KEY",
                "BYBIT_TESTNET_SECRET",
                None,
            ),
            okx: ExchangeVenueConfig::new(
                "OKX_HTTP_URL",
                "https://www.okx.com",
                "OKX_WS_URL",
                "wss://ws.okx.com:8443/ws/v5/public",
                "OKX_API_KEY",
                "OKX_SECRET",
                Some("OKX_PASSPHRASE"),
            ),
            coinbase: ExchangeVenueConfig::new(
                "COINBASE_HTTP_URL",
                "https://api.coinbase.com/api/v3/brokerage",
                "COINBASE_WS_URL",
                "wss://advanced-trade-ws.coinbase.com",
                "COINBASE_API_KEY",
                "COINBASE_SECRET",
                None,
            ),
            kraken: ExchangeVenueConfig::new(
                "KRAKEN_HTTP_URL",
                "https://api.kraken.com",
                "KRAKEN_WS_URL",
                "wss://ws.kraken.com/v2",
                "KRAKEN_API_KEY",
                "KRAKEN_SECRET",
                None,
            ),
            production_fee_bps: env_parse("CEX_FEE_BPS_PRODUCTION", 10.0),
            testnet_fee_bps: env_parse("CEX_FEE_BPS_TESTNET", 0.0),
            default_pair: env_string("DEFAULT_PAIR", "ETH/USDC"),
            is_sandbox: env_bool("EXCHANGE_SANDBOX", false),
            skip_connection_validation: env_bool("EXCHANGE_SKIP_CONNECTION_VALIDATION", false),
        }
    }

    pub fn fee_bps(&self, production: bool) -> f64 {
        if production {
            self.production_fee_bps
        } else {
            self.testnet_fee_bps
        }
    }
}

impl Default for ExchangeSettings {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct ExchangeVenueConfig {
    pub http_url: String,
    pub ws_url: String,
    pub api_key: SecretValue,
    pub secret: SecretValue,
    pub passphrase: SecretValue,
}

impl ExchangeVenueConfig {
    fn new(
        http_env: &str,
        default_http_url: &str,
        ws_env: &str,
        default_ws_url: &str,
        key_env: &str,
        secret_env: &str,
        passphrase_env: Option<&str>,
    ) -> Self {
        Self {
            http_url: env_string(http_env, default_http_url),
            ws_url: env_string(ws_env, default_ws_url),
            api_key: SecretValue::from_env(key_env),
            secret: SecretValue::from_env(secret_env),
            passphrase: passphrase_env
                .map(SecretValue::from_env)
                .unwrap_or_default(),
        }
    }
}

impl fmt::Debug for ExchangeVenueConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExchangeVenueConfig")
            .field("http_url", &self.http_url)
            .field("ws_url", &self.ws_url)
            .field("api_key", &self.api_key)
            .field("secret", &self.secret)
            .field("passphrase", &self.passphrase)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct TokenConfig {
    pub arbitrum_weth: String,
    pub arbitrum_usdt: String,
    pub arbitrum_usdc: String,
    pub pepe: String,
    pub ethereum_weth: String,
    pub ethereum_usdt: String,
    pub ethereum_usdc: String,
}

impl TokenConfig {
    pub fn new() -> Self {
        Self {
            arbitrum_weth: env_string(
                "ARBITRUM_WETH",
                "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            ),
            arbitrum_usdt: env_string(
                "ARBITRUM_USDT",
                "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
            ),
            arbitrum_usdc: env_string(
                "ARBITRUM_USDC",
                "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            ),
            pepe: env_string("PEPE_TOKEN", "0x25d887Ce7a35172C62FeBFD67a1856F20FaEbB00"),
            ethereum_weth: env_string(
                "ETHEREUM_WETH",
                "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            ),
            ethereum_usdt: env_string(
                "ETHEREUM_USDT",
                "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            ),
            ethereum_usdc: env_string(
                "ETHEREUM_USDC",
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            ),
        }
    }
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct DexConfig {
    pub ethereum: DexNetworkConfig,
    pub arbitrum: DexNetworkConfig,
}

impl DexConfig {
    pub fn new() -> Self {
        Self {
            ethereum: DexNetworkConfig::ethereum(),
            arbitrum: DexNetworkConfig::arbitrum(),
        }
    }
}

impl Default for DexConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct DexNetworkConfig {
    pub network: String,
    pub chain_id: u64,
    pub uniswap_v2: UniswapV2Config,
    pub uniswap_v3: UniswapV3Config,
    pub pools: DexPoolConfig,
}

impl DexNetworkConfig {
    fn ethereum() -> Self {
        Self {
            network: "ethereum".to_string(),
            chain_id: env_parse("ETHEREUM_CHAIN_ID", 1),
            uniswap_v2: UniswapV2Config {
                factory: env_string(
                    "ETHEREUM_UNISWAP_V2_FACTORY",
                    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                ),
                router: env_string(
                    "ETHEREUM_UNISWAP_V2_ROUTER",
                    "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
                ),
            },
            uniswap_v3: UniswapV3Config {
                factory: env_string(
                    "ETHEREUM_UNISWAP_V3_FACTORY",
                    "0x1F98431c8aD98523631AE4a59f267346ea31F984",
                ),
                router: env_string(
                    "ETHEREUM_UNISWAP_V3_ROUTER",
                    "0xE592427A0AEce92De3Edee1F18E0157C05861564",
                ),
                swap_router_02: env_string(
                    "ETHEREUM_UNISWAP_V3_SWAP_ROUTER_02",
                    "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
                ),
                quoter: env_string(
                    "ETHEREUM_UNISWAP_V3_QUOTER",
                    "0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6",
                ),
                quoter_v2: env_string(
                    "ETHEREUM_UNISWAP_V3_QUOTER_V2",
                    "0x61fFE014bA17989E743c5F6cB21bF9697530B21e",
                ),
                universal_router: env_string(
                    "ETHEREUM_UNISWAP_UNIVERSAL_ROUTER",
                    "0x66a9893cC07D91D95644AEDD05D03f95e1dBA8Af",
                ),
                position_manager: env_string(
                    "ETHEREUM_UNISWAP_V3_POSITION_MANAGER",
                    "0xC36442b4a4522E871399CD717aBDD847Ab11FE88",
                ),
            },
            pools: DexPoolConfig {
                weth_usdc_primary: Some(env_string(
                    "ETHEREUM_WETH_USDC_POOL",
                    "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8",
                )),
                weth_usdt_primary: env_optional("ETHEREUM_WETH_USDT_POOL"),
                pepe_usdt_primary: env_optional("ETHEREUM_PEPE_USDT_POOL"),
            },
        }
    }

    fn arbitrum() -> Self {
        Self {
            network: "arbitrum".to_string(),
            chain_id: env_parse("ARBITRUM_CHAIN_ID", 42_161),
            uniswap_v2: UniswapV2Config {
                factory: env_string(
                    "ARBITRUM_UNISWAP_V2_FACTORY",
                    "0xf1D7CC64Fb4452F05c498126312eBE29f30Fbcf9",
                ),
                router: env_string(
                    "ARBITRUM_UNISWAP_V2_ROUTER",
                    "0x4752ba5dbc23f44d87826276bf6fd6b1c372ad24",
                ),
            },
            uniswap_v3: UniswapV3Config {
                factory: env_string(
                    "ARBITRUM_UNISWAP_V3_FACTORY",
                    "0x1F98431c8aD98523631AE4a59f267346ea31F984",
                ),
                router: env_string_any(
                    &["ARBITRUM_UNISWAP_V3_ROUTER", "ARBITRUM_ROUTER"],
                    "0xE592427A0AEce92De3Edee1F18E0157C05861564",
                ),
                swap_router_02: env_string(
                    "ARBITRUM_UNISWAP_V3_SWAP_ROUTER_02",
                    "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45",
                ),
                quoter: env_string(
                    "ARBITRUM_UNISWAP_V3_QUOTER",
                    "0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6",
                ),
                quoter_v2: env_string(
                    "ARBITRUM_UNISWAP_V3_QUOTER_V2",
                    "0x61fFE014bA17989E743c5F6cB21bF9697530B21e",
                ),
                universal_router: env_string(
                    "ARBITRUM_UNISWAP_UNIVERSAL_ROUTER",
                    "0xa51afafe0263b40edaef0df8781ea9aa03e381a3",
                ),
                position_manager: env_string(
                    "ARBITRUM_UNISWAP_V3_POSITION_MANAGER",
                    "0xC36442b4a4522E871399CD717aBDD847Ab11FE88",
                ),
            },
            pools: DexPoolConfig {
                weth_usdc_primary: env_optional("ARBITRUM_WETH_USDC_POOL"),
                weth_usdt_primary: Some(env_string_any(
                    &["ARBITRUM_WETH_USDT_POOL", "WETH_USDT_POOL"],
                    "0x641c00a822e8b671738d32a431a4fb6074e5c79d",
                )),
                pepe_usdt_primary: Some(env_string_any(
                    &["ARBITRUM_PEPE_USDT_POOL", "PEPE_USDT_POOL"],
                    "0xcd518702e7d8f16d8a6407bf5d22e0f0eec15162",
                )),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct UniswapV2Config {
    pub factory: String,
    pub router: String,
}

#[derive(Debug, Clone)]
pub struct UniswapV3Config {
    pub factory: String,
    pub router: String,
    pub swap_router_02: String,
    pub quoter: String,
    pub quoter_v2: String,
    pub universal_router: String,
    pub position_manager: String,
}

#[derive(Debug, Clone, Default)]
pub struct DexPoolConfig {
    pub weth_usdc_primary: Option<String>,
    pub weth_usdt_primary: Option<String>,
    pub pepe_usdt_primary: Option<String>,
}

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: SecretValue,
    pub chat_id: Option<String>,
}

impl TelegramConfig {
    pub fn new() -> Self {
        Self {
            bot_token: SecretValue::from_env("TELEGRAM_BOT_TOKEN"),
            chat_id: env_optional("TELEGRAM_CHAT_ID"),
        }
    }
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("bot_token", &self.bot_token)
            .field("chat_id", &self.chat_id)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct FlashbotsConfig {
    pub relay_url: String,
    pub auth_key: SecretValue,
}

impl FlashbotsConfig {
    pub fn new() -> Self {
        Self {
            relay_url: env_string("FLASHBOTS_RELAY_URL", "https://relay.flashbots.net"),
            auth_key: SecretValue::from_env_any(&["FLASHBOTS_AUTH_KEY", "PRIVATE_KEY"]),
        }
    }
}

impl Default for FlashbotsConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct SecretValue {
    value: Option<String>,
}

impl SecretValue {
    pub fn from_env(key: &str) -> Self {
        Self {
            value: env_optional(key),
        }
    }

    pub fn as_deref(&self) -> Option<&str> {
        self.value.as_deref()
    }

    pub fn clone_value(&self) -> Option<String> {
        self.value.clone()
    }

    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }

    pub fn from_env_any(keys: &[&str]) -> Self {
        Self {
            value: env_optional_any(keys),
        }
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_set() {
            f.write_str("<redacted>")
        } else {
            f.write_str("<unset>")
        }
    }
}

fn env_optional(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn env_string(key: &str, default: &str) -> String {
    env_optional(key).unwrap_or_else(|| default.to_string())
}

fn env_optional_any(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| env_optional(key))
}

fn env_string_any(keys: &[&str], default: &str) -> String {
    env_optional_any(keys).unwrap_or_else(|| default.to_string())
}

fn env_parse<T>(key: &str, default: T) -> T
where
    T: FromStr,
{
    env_optional(key)
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_optional(key).map(|value| value.to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "y" | "on") => true,
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "n" | "off") => false,
        _ => default,
    }
}
