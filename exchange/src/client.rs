use arb_core::error::ArbError;
use arb_core::types::TokenAmount;
use base64::{Engine as _, engine::general_purpose};
use chrono::{SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use openssl::{ecdsa::EcdsaSig, hash::MessageDigest, pkey::PKey, sign::Signer};
use reqwest::Client;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;

pub struct RateLimiter {
    capacity: f64,
    pub tokens: Arc<Mutex<f64>>,
    last_fill: Arc<Mutex<Instant>>,
    pub tokens_per_ms: f64,
}

impl RateLimiter {
    pub fn new(max_weight_per_minute: u64) -> Self {
        let capacity = max_weight_per_minute as f64;
        Self {
            capacity,
            tokens: Arc::new(Mutex::new(capacity)),
            last_fill: Arc::new(Mutex::new(Instant::now())),
            tokens_per_ms: capacity / 60_000.0,
        }
    }

    pub async fn update_from_headers(&self, used_weight: f64) {
        let mut tokens = self.tokens.lock().await;
        let remaining = (self.capacity - used_weight).max(0.0);
        *tokens = remaining;
    }

    pub async fn wait(&self, weight: u32) {
        let weight_f = weight as f64;
        loop {
            let now = Instant::now();
            let mut tokens = self.tokens.lock().await;
            let mut last_fill = self.last_fill.lock().await;

            let elapsed = now.duration_since(*last_fill).as_secs_f64() * 1000.0;
            let refill = elapsed * self.tokens_per_ms;

            if refill > 0.0 {
                *tokens = (*tokens + refill).min(self.capacity);
                *last_fill = now;
            }

            if *tokens >= weight_f {
                *tokens -= weight_f;
                return;
            }

            let missing = weight_f - *tokens;
            let wait_ms = (missing / self.tokens_per_ms).ceil() as u64;

            drop(tokens);
            drop(last_fill);

            tokio::time::sleep(Duration::from_millis(wait_ms.max(1))).await;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub symbol: String,
    pub timestamp: u64,
    pub bids: Vec<(Decimal, Decimal)>,
    pub asks: Vec<(Decimal, Decimal)>,
    pub best_bid: (Decimal, Decimal),
    pub best_ask: (Decimal, Decimal),
    pub mid_price: Decimal,
    pub spread_bps: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub free: TokenAmount,
    pub locked: TokenAmount,
    pub total: TokenAmount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResult {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub time_in_force: String,
    pub amount_requested: TokenAmount,
    pub amount_filled: TokenAmount,
    pub avg_fill_price: Decimal,
    pub fee: TokenAmount,
    pub status: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExchangeType {
    Binance,
    Bybit,
    Okx,
    Coinbase,
    Kraken,
}

pub struct ExchangeClient {
    pub config: crate::config::ExchangeConfig,
    pub exchange_type: ExchangeType,
    pub http_client: Client,
    pub base_url: String,
    pub ws_url: String,
    pub rate_limiter: RateLimiter,
}

impl ExchangeClient {
    pub async fn new(
        mut config: crate::config::ExchangeConfig,
        exchange_type: ExchangeType,
    ) -> Result<Self, ArbError> {
        config.api_key = config.api_key.trim().to_string();
        config.secret = config.secret.trim().to_string();

        let (base_url, ws_url, rate_limit) = match exchange_type {
            ExchangeType::Binance => (
                config.binance_http_url.clone(),
                config.binance_ws_url.clone(),
                1200,
            ),
            ExchangeType::Bybit => (
                config.binance_http_url.clone(),
                config.binance_ws_url.clone(),
                600,
            ),
            ExchangeType::Okx => (
                config.binance_http_url.clone(),
                config.binance_ws_url.clone(),
                600,
            ),
            ExchangeType::Coinbase => (
                config.binance_http_url.clone(),
                config.binance_ws_url.clone(),
                600,
            ),
            ExchangeType::Kraken => (
                config.binance_http_url.clone(),
                config.binance_ws_url.clone(),
                900,
            ),
        };

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        let exchange = Self {
            config,
            exchange_type,
            http_client: client,
            base_url,
            ws_url,
            rate_limiter: RateLimiter::new(rate_limit),
        };

        if !exchange.config.skip_connection_validation {
            exchange.validate_connection().await?;
        }

        Ok(exchange)
    }

    fn get_weight(&self, endpoint: &str, priority: RequestPriority) -> u32 {
        match (self.exchange_type, priority) {
            (ExchangeType::Binance, RequestPriority::High) => 1,
            (ExchangeType::Binance, RequestPriority::Medium) => {
                if endpoint.contains("/depth") {
                    1
                } else {
                    2
                }
            }
            (ExchangeType::Binance, RequestPriority::Low) => 10,
            (ExchangeType::Bybit, RequestPriority::High) => 1,
            _ => 1,
        }
    }

    async fn signed_request(
        &self,
        method: &str,
        endpoint: &str,
        params: &str,
        priority: RequestPriority,
    ) -> Result<serde_json::Value, ArbError> {
        let weight = self.get_weight(endpoint, priority);
        self.rate_limiter.wait(weight).await;

        match self.exchange_type {
            ExchangeType::Okx => return self.okx_signed_request(method, endpoint, params).await,
            ExchangeType::Coinbase => {
                return self.coinbase_signed_request(method, endpoint, params).await;
            }
            ExchangeType::Kraken => {
                return self.kraken_signed_request(method, endpoint, params).await;
            }
            ExchangeType::Binance | ExchangeType::Bybit => {}
        }

        let timestamp = Self::get_timestamp();
        let recv_window = 5000;
        let api_key = self.config.api_key.trim();

        let (url, body, signature) = match self.exchange_type {
            ExchangeType::Binance => {
                let query = if params.is_empty() {
                    format!("timestamp={}", timestamp)
                } else {
                    format!("{}&timestamp={}", params, timestamp)
                };
                let signature = self.sign_query(&query);
                (
                    format!(
                        "{}{}?{}&signature={}",
                        self.base_url, endpoint, query, signature
                    ),
                    None,
                    None,
                )
            }
            ExchangeType::Bybit => {
                let (url, payload_to_sign, body_data) = if method == "GET" {
                    let url = format!("{}{}?{}", self.base_url, endpoint, params);
                    (url, params.to_string(), None)
                } else {
                    (
                        format!("{}{}", self.base_url, endpoint),
                        params.to_string(),
                        Some(params.to_string()),
                    )
                };
                let signature_payload =
                    format!("{}{}{}{}", timestamp, api_key, recv_window, payload_to_sign);
                let signature = self.sign_query(&signature_payload);
                (url, body_data, Some(signature))
            }
            ExchangeType::Okx | ExchangeType::Coinbase | ExchangeType::Kraken => unreachable!(),
        };

        let mut rb = self.request_builder(method, &url)?;

        if self.exchange_type == ExchangeType::Binance {
            rb = rb.header("X-MBX-APIKEY", api_key);
        } else {
            rb = rb
                .header("X-BAPI-API-KEY", api_key)
                .header("X-BAPI-SIGN", signature.unwrap_or_default())
                .header("X-BAPI-TIMESTAMP", timestamp.to_string())
                .header("X-BAPI-RECV-WINDOW", recv_window.to_string());
            if method != "GET" {
                rb = rb.header("Content-Type", "application/json");
            }
        }

        if let Some(b) = body {
            rb = rb.body(b);
        }

        let resp = rb
            .send()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        if self.exchange_type == ExchangeType::Binance
            && let Some(used) = resp.headers().get("X-MBX-USED-WEIGHT-1M")
            && let Ok(val) = used.to_str().unwrap_or("0").parse::<f64>()
        {
            self.rate_limiter.update_from_headers(val).await;
        }

        let data = self.parse_http_response(resp).await?;
        if self.exchange_type == ExchangeType::Bybit && data["retCode"].as_i64().unwrap_or(0) != 0 {
            return Err(ArbError::SerializationError(format!(
                "Bybit private API error: {}",
                data
            )));
        }
        Ok(data)
    }

    async fn okx_signed_request(
        &self,
        method: &str,
        endpoint: &str,
        params: &str,
    ) -> Result<serde_json::Value, ArbError> {
        let request_path = if method == "GET" && !params.is_empty() {
            format!("{}?{}", endpoint, params)
        } else {
            endpoint.to_string()
        };
        let body = if method == "GET" { "" } else { params };
        let timestamp = Self::okx_timestamp();
        let signature = self.sign_okx(&timestamp, method, &request_path, body)?;
        let url = format!("{}{}", self.base_url, request_path);

        let mut rb = self
            .request_builder(method, &url)?
            .header("OK-ACCESS-KEY", self.config.api_key.trim())
            .header("OK-ACCESS-SIGN", signature)
            .header("OK-ACCESS-TIMESTAMP", timestamp)
            .header(
                "OK-ACCESS-PASSPHRASE",
                self.config.passphrase.as_deref().unwrap_or(""),
            )
            .header("Accept", "application/json");

        if self.config.is_sandbox {
            rb = rb.header("x-simulated-trading", "1");
        }

        if method != "GET" {
            rb = rb
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }

        let data = self
            .parse_http_response(
                rb.send()
                    .await
                    .map_err(|e| ArbError::SerializationError(e.to_string()))?,
            )
            .await?;

        if data["code"].as_str().unwrap_or("0") != "0" {
            return Err(ArbError::SerializationError(format!(
                "OKX private API error: {}",
                data
            )));
        }
        Ok(data)
    }

    async fn coinbase_signed_request(
        &self,
        method: &str,
        endpoint: &str,
        params: &str,
    ) -> Result<serde_json::Value, ArbError> {
        let query = if method == "GET" && !params.is_empty() {
            Some(params)
        } else {
            None
        };
        let request_path = self.coinbase_request_path(endpoint, query)?;
        let url = if let Some(query) = query {
            format!("{}{}?{}", self.base_url, endpoint, query)
        } else {
            format!("{}{}", self.base_url, endpoint)
        };
        let token = self.coinbase_jwt(method, &request_path)?;

        let mut rb = self
            .request_builder(method, &url)?
            .bearer_auth(token)
            .header("Accept", "application/json");

        if method != "GET" {
            rb = rb
                .header("Content-Type", "application/json")
                .body(params.to_string());
        }

        let data = self
            .parse_http_response(
                rb.send()
                    .await
                    .map_err(|e| ArbError::SerializationError(e.to_string()))?,
            )
            .await?;

        if data["success"].as_bool() == Some(false) {
            return Err(ArbError::SerializationError(format!(
                "Coinbase private API error: {}",
                data
            )));
        }
        Ok(data)
    }

    async fn kraken_signed_request(
        &self,
        method: &str,
        endpoint: &str,
        params: &str,
    ) -> Result<serde_json::Value, ArbError> {
        if method != "POST" {
            return Err(ArbError::SerializationError(
                "Kraken private REST endpoints require POST".to_string(),
            ));
        }

        let nonce = Self::get_timestamp().to_string();
        let body = if params.is_empty() {
            format!("nonce={}", nonce)
        } else {
            format!("nonce={}&{}", nonce, params)
        };
        let signature = self.sign_kraken(endpoint, &nonce, &body)?;
        let url = format!("{}{}", self.base_url, endpoint);

        let data = self
            .parse_http_response(
                self.http_client
                    .post(&url)
                    .header("API-Key", self.config.api_key.trim())
                    .header("API-Sign", signature)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| ArbError::SerializationError(e.to_string()))?,
            )
            .await?;

        if let Some(errors) = data["error"].as_array()
            && !errors.is_empty()
        {
            return Err(ArbError::SerializationError(format!(
                "Kraken private API error: {}",
                data
            )));
        }
        Ok(data)
    }

    pub async fn fetch_balance(&self) -> Result<HashMap<String, Balance>, ArbError> {
        let (method, endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => ("GET", "/api/v3/account", "".to_string()),
            ExchangeType::Bybit => (
                "GET",
                "/v5/account/wallet-balance",
                "accountType=UNIFIED".to_string(),
            ),
            ExchangeType::Okx => ("GET", "/api/v5/account/balance", "".to_string()),
            ExchangeType::Coinbase => ("GET", "/accounts", "".to_string()),
            ExchangeType::Kraken => ("POST", "/0/private/Balance", "".to_string()),
        };

        let data = self
            .signed_request(method, endpoint, &params, RequestPriority::Low)
            .await?;
        let mut balances = HashMap::new();

        match self.exchange_type {
            ExchangeType::Binance | ExchangeType::Bybit => {
                let assets = if self.exchange_type == ExchangeType::Binance {
                    data["balances"].as_array()
                } else {
                    data["result"]["list"][0]["coin"].as_array()
                };

                if let Some(assets) = assets {
                    for asset in assets {
                        let symbol = asset["asset"]
                            .as_str()
                            .or(asset["coin"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let free_str = asset["free"]
                            .as_str()
                            .or(asset["equity"].as_str())
                            .or(asset["walletBalance"].as_str())
                            .unwrap_or("0");
                        let locked_str = asset["locked"].as_str().unwrap_or("0");

                        let free = TokenAmount::from_human(free_str, 8, Some(symbol.clone()))?;
                        let locked = TokenAmount::from_human(locked_str, 8, Some(symbol.clone()))?;
                        let total = (free.clone() + locked.clone())?;

                        if total.raw > alloy_primitives::U256::ZERO {
                            balances.insert(
                                symbol,
                                Balance {
                                    free,
                                    locked,
                                    total,
                                },
                            );
                        }
                    }
                }
            }
            ExchangeType::Okx => {
                if let Some(assets) = data["data"][0]["details"].as_array() {
                    for asset in assets {
                        let symbol = asset["ccy"].as_str().unwrap_or("").to_string();
                        let free_str = asset["availBal"].as_str().unwrap_or("0");
                        let locked_str = asset["frozenBal"].as_str().unwrap_or("0");
                        let total_str = asset["cashBal"]
                            .as_str()
                            .or(asset["eq"].as_str())
                            .unwrap_or(free_str);

                        let free = TokenAmount::from_human(free_str, 8, Some(symbol.clone()))?;
                        let locked = TokenAmount::from_human(locked_str, 8, Some(symbol.clone()))?;
                        let total = TokenAmount::from_human(total_str, 8, Some(symbol.clone()))
                            .or_else(|_| free.clone() + locked.clone())?;

                        if total.raw > alloy_primitives::U256::ZERO {
                            balances.insert(
                                symbol,
                                Balance {
                                    free,
                                    locked,
                                    total,
                                },
                            );
                        }
                    }
                }
            }
            ExchangeType::Coinbase => {
                if let Some(accounts) = data["accounts"].as_array() {
                    for account in accounts {
                        let symbol = account["currency"].as_str().unwrap_or("").to_string();
                        let free_str = account["available_balance"]["value"]
                            .as_str()
                            .unwrap_or("0");
                        let locked_str = account["hold"]["value"].as_str().unwrap_or("0");

                        let free = TokenAmount::from_human(free_str, 8, Some(symbol.clone()))?;
                        let locked = TokenAmount::from_human(locked_str, 8, Some(symbol.clone()))?;
                        let total = (free.clone() + locked.clone())?;

                        if total.raw > alloy_primitives::U256::ZERO {
                            balances.insert(
                                symbol,
                                Balance {
                                    free,
                                    locked,
                                    total,
                                },
                            );
                        }
                    }
                }
            }
            ExchangeType::Kraken => {
                if let Some(assets) = data["result"].as_object() {
                    for (symbol, amount) in assets {
                        let amount_str = amount.as_str().unwrap_or("0");
                        let free = TokenAmount::from_human(amount_str, 8, Some(symbol.clone()))?;
                        let locked = TokenAmount::from_human("0", 8, Some(symbol.clone()))?;
                        let total = free.clone();

                        if total.raw > alloy_primitives::U256::ZERO {
                            balances.insert(
                                symbol.clone(),
                                Balance {
                                    free,
                                    locked,
                                    total,
                                },
                            );
                        }
                    }
                }
            }
        }
        Ok(balances)
    }

    pub async fn create_limit_ioc_order(
        &self,
        symbol: &str,
        side: &str,
        amount: TokenAmount,
        price: Decimal,
    ) -> Result<OrderResult, ArbError> {
        let symbol_clean = self.normalize_symbol(symbol);
        let amount_str = amount.to_human().normalize().to_string();
        let price_str = price.normalize().to_string();
        let side_upper = side.to_uppercase();
        let side_lower = side.to_lowercase();
        let (endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => (
                "/api/v3/order",
                format!("symbol={}&side={}&type=LIMIT&timeInForce=IOC&quantity={}&price={}",
                symbol_clean, side_upper, amount_str, price_str)
            ),
            ExchangeType::Bybit => (
                "/v5/order/create",
                serde_json::json!({
                    "category": "spot", "symbol": symbol_clean, "side": side_upper,
                    "orderType": "Limit", "qty": amount_str, "price": price_str, "timeInForce": "IOC"
                }).to_string()
            ),
            ExchangeType::Okx => (
                "/api/v5/trade/order",
                serde_json::json!({
                    "instId": symbol_clean,
                    "tdMode": "cash",
                    "clOrdId": Self::client_order_id("arb"),
                    "side": side_lower,
                    "ordType": "ioc",
                    "px": price_str,
                    "sz": amount_str
                }).to_string(),
            ),
            ExchangeType::Coinbase => (
                "/orders",
                serde_json::json!({
                    "client_order_id": Self::client_order_id("arb"),
                    "product_id": symbol_clean,
                    "side": side_upper,
                    "order_configuration": {
                        "sor_limit_ioc": {
                            "base_size": amount_str,
                            "limit_price": price_str,
                            "rfq_disabled": true
                        }
                    }
                }).to_string(),
            ),
            ExchangeType::Kraken => (
                "/0/private/AddOrder",
                Self::encode_form(&[
                    ("pair", symbol_clean),
                    ("type", side_lower),
                    ("ordertype", "limit".to_string()),
                    ("volume", amount_str),
                    ("price", price_str),
                    ("timeinforce", "IOC".to_string()),
                ]),
            ),
        };

        let data = self
            .signed_request("POST", endpoint, &params, RequestPriority::High)
            .await?;
        self.map_order_response_with_request(
            data,
            symbol,
            Some(side.to_lowercase()),
            Some("limit".to_string()),
            Some("IOC".to_string()),
            Some(amount),
            Some(price),
        )
    }

    pub async fn fetch_order_book(&self, symbol: &str, limit: u32) -> Result<OrderBook, ArbError> {
        let mut symbol_clean = symbol.replace("/", "");

        if self.exchange_type == ExchangeType::Binance {
            symbol_clean = symbol_clean.replace("WETH", "ETH");
            symbol_clean = symbol_clean.replace("WBTC", "BTC");
        }

        let (url, bid_key, ask_key, data_path) = match self.exchange_type {
            ExchangeType::Binance => (
                format!(
                    "{}/api/v3/depth?symbol={}&limit={}",
                    self.base_url, symbol_clean, limit
                ),
                "bids",
                "asks",
                None,
            ),
            ExchangeType::Bybit => (
                format!(
                    "{}/v5/market/orderbook?category=spot&symbol={}&limit={}",
                    self.base_url, symbol_clean, limit
                ),
                "b",
                "a",
                Some("result"),
            ),
            ExchangeType::Okx => {
                symbol_clean = symbol.replace("/", "-").to_uppercase();
                (
                    format!(
                        "{}/api/v5/market/books?instId={}&sz={}",
                        self.base_url, symbol_clean, limit
                    ),
                    "bids",
                    "asks",
                    Some("okx_data"),
                )
            }
            ExchangeType::Coinbase => {
                symbol_clean = symbol.replace("/", "-").to_uppercase();
                (
                    format!(
                        "{}/market/product_book?product_id={}&limit={}",
                        self.base_url, symbol_clean, limit
                    ),
                    "bids",
                    "asks",
                    Some("pricebook"),
                )
            }
            ExchangeType::Kraken => (
                format!(
                    "{}/0/public/Depth?pair={}&count={}",
                    self.base_url, symbol_clean, limit
                ),
                "bids",
                "asks",
                Some("kraken_result"),
            ),
        };

        self.rate_limiter
            .wait(self.get_weight("/depth", RequestPriority::Medium))
            .await;

        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;
        let mut data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        if let Some(path) = data_path {
            data = match path {
                "okx_data" => data["data"].get(0).cloned().unwrap_or_default(),
                "kraken_result" => data["result"]
                    .as_object()
                    .and_then(|result| result.values().next().cloned())
                    .unwrap_or_default(),
                _ => data[path].clone(),
            };
        }

        let bids = self.parse_levels(&data[bid_key])?;
        let asks = self.parse_levels(&data[ask_key])?;

        Ok(self.construct_order_book(symbol.to_string(), bids, asks))
    }

    pub async fn stream_order_book<F>(&self, symbol: &str, callback: F) -> Result<(), ArbError>
    where
        F: FnMut(OrderBook) + Send + 'static,
    {
        match self.exchange_type {
            ExchangeType::Binance => self.stream_binance(symbol, callback).await,
            ExchangeType::Bybit => self.stream_bybit(symbol, callback).await,
            ExchangeType::Okx | ExchangeType::Coinbase | ExchangeType::Kraken => {
                Err(Self::unsupported_streaming(self.exchange_type))
            }
        }
    }

    async fn stream_binance<F>(&self, symbol: &str, mut callback: F) -> Result<(), ArbError>
    where
        F: FnMut(OrderBook) + Send + 'static,
    {
        let symbol_clean = symbol.replace("/", "").to_lowercase();
        let stream_name = format!("{}@depth20@100ms", symbol_clean);
        let ws_endpoint = format!("{}/{}", self.ws_url, stream_name);

        let (mut ws_stream, _) = connect_async(&ws_endpoint)
            .await
            .map_err(|e| ArbError::SerializationError(format!("Binance WS Connect: {}", e)))?;
        while let Some(message) = ws_stream.next().await {
            if let Ok(Message::Text(text)) = message
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&text)
            {
                let bids = self.parse_levels(&data["bids"])?;
                let asks = self.parse_levels(&data["asks"])?;

                if !bids.is_empty() && !asks.is_empty() {
                    callback(self.construct_order_book(symbol.to_string(), bids, asks));
                }
            }
        }
        Ok(())
    }

    async fn stream_bybit<F>(&self, symbol: &str, mut callback: F) -> Result<(), ArbError>
    where
        F: FnMut(OrderBook) + Send + 'static,
    {
        let symbol_clean = symbol.replace("/", "").to_uppercase();
        let (mut ws_stream, _) = connect_async(&self.ws_url)
            .await
            .map_err(|e| ArbError::SerializationError(format!("Bybit WS Connect: {}", e)))?;

        let subscribe_msg = serde_json::json!({
            "op": "subscribe",
            "args": [format!("orderbook.1.{}", symbol_clean)]
        });

        ws_stream
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        while let Some(message) = ws_stream.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text)
                        && let Some(ob_data) = data.get("data")
                    {
                        let bids = self.parse_levels(&ob_data["b"])?;
                        let asks = self.parse_levels(&ob_data["a"])?;

                        if !bids.is_empty() && !asks.is_empty() {
                            callback(self.construct_order_book(symbol.to_string(), bids, asks));
                        }
                    }
                }
                Ok(Message::Ping(p)) => {
                    let _ = ws_stream.send(Message::Pong(p)).await;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub async fn cancel_order(&self, symbol: &str, order_id: &str) -> Result<bool, ArbError> {
        let symbol_clean = self.normalize_symbol(symbol);
        let (method, endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => (
                "DELETE",
                "/api/v3/order",
                format!("symbol={}&orderId={}", symbol_clean, order_id),
            ),
            ExchangeType::Bybit => (
                "POST",
                "/v5/order/cancel",
                serde_json::json!({"category": "spot", "symbol": symbol_clean, "orderId": order_id}).to_string(),
            ),
            ExchangeType::Okx => (
                "POST",
                "/api/v5/trade/cancel-order",
                serde_json::json!({"instId": symbol_clean, "ordId": order_id}).to_string(),
            ),
            ExchangeType::Coinbase => (
                "POST",
                "/orders/batch_cancel",
                serde_json::json!({"order_ids": [order_id]}).to_string(),
            ),
            ExchangeType::Kraken => (
                "POST",
                "/0/private/CancelOrder",
                Self::encode_form(&[("txid", order_id.to_string())]),
            ),
        };

        let data = self
            .signed_request(method, endpoint, &params, RequestPriority::High)
            .await?;
        Ok(match self.exchange_type {
            ExchangeType::Binance => {
                data["orderId"].as_str().is_some() || data["orderId"].as_u64().is_some()
            }
            ExchangeType::Bybit => data["retCode"].as_i64().unwrap_or(-1) == 0,
            ExchangeType::Okx => data["data"][0]["sCode"].as_str().unwrap_or("-1") == "0",
            ExchangeType::Coinbase => data["results"][0]["success"].as_bool().unwrap_or(false),
            ExchangeType::Kraken => data["result"]["count"].as_u64().unwrap_or(0) > 0,
        })
    }

    fn sign_query(&self, payload: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(self.config.secret.as_bytes()).expect("HMAC Error");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn sign_okx(
        &self,
        timestamp: &str,
        method: &str,
        request_path: &str,
        body: &str,
    ) -> Result<String, ArbError> {
        let prehash = format!(
            "{}{}{}{}",
            timestamp,
            method.to_uppercase(),
            request_path,
            body
        );
        let mut mac = HmacSha256::new_from_slice(self.config.secret.as_bytes())
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;
        mac.update(prehash.as_bytes());
        Ok(general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
    }

    fn sign_kraken(&self, endpoint: &str, nonce: &str, body: &str) -> Result<String, ArbError> {
        let mut sha = Sha256::new();
        sha.update(format!("{}{}", nonce, body).as_bytes());
        let hash = sha.finalize();

        let mut message = endpoint.as_bytes().to_vec();
        message.extend_from_slice(&hash);

        let secret = general_purpose::STANDARD
            .decode(self.config.secret.trim())
            .map_err(|e| ArbError::SerializationError(format!("Kraken secret decode: {}", e)))?;
        let mut mac = HmacSha512::new_from_slice(&secret)
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;
        mac.update(&message);
        Ok(general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
    }

    fn coinbase_jwt(&self, method: &str, request_path: &str) -> Result<String, ArbError> {
        #[derive(Serialize)]
        struct CoinbaseJwtHeader<'a> {
            alg: &'static str,
            typ: &'static str,
            kid: &'a str,
            nonce: String,
        }

        #[derive(Serialize)]
        struct CoinbaseJwtClaims<'a> {
            sub: &'a str,
            iss: &'static str,
            nbf: u64,
            exp: u64,
            uri: String,
        }

        let key_name = self.config.api_key.trim();
        let host = self.base_url_host()?;
        let now = Self::get_timestamp() / 1000;
        let uri = format!("{} {}{}", method.to_uppercase(), host, request_path);
        let header = CoinbaseJwtHeader {
            alg: "ES256",
            typ: "JWT",
            kid: key_name,
            nonce: Self::nonce_hex(),
        };
        let claims = CoinbaseJwtClaims {
            sub: key_name,
            iss: "cdp",
            nbf: now,
            exp: now + 120,
            uri,
        };

        let encoded_header = general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&header).map_err(|e| ArbError::SerializationError(e.to_string()))?,
        );
        let encoded_claims = general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&claims).map_err(|e| ArbError::SerializationError(e.to_string()))?,
        );
        let signing_input = format!("{}.{}", encoded_header, encoded_claims);

        let secret = self.config.secret.replace("\\n", "\n");
        let key = PKey::private_key_from_pem(secret.as_bytes())
            .map_err(|e| ArbError::SerializationError(format!("Coinbase JWT key: {}", e)))?;
        let mut signer = Signer::new(MessageDigest::sha256(), &key)
            .map_err(|e| ArbError::SerializationError(format!("Coinbase JWT signer: {}", e)))?;
        signer
            .update(signing_input.as_bytes())
            .map_err(|e| ArbError::SerializationError(format!("Coinbase JWT update: {}", e)))?;
        let der_signature = signer
            .sign_to_vec()
            .map_err(|e| ArbError::SerializationError(format!("Coinbase JWT sign: {}", e)))?;
        let signature = EcdsaSig::from_der(&der_signature)
            .map_err(|e| ArbError::SerializationError(format!("Coinbase JWT DER: {}", e)))?;

        let mut raw_signature = Vec::with_capacity(64);
        Self::append_es256_coord(&mut raw_signature, signature.r().to_vec())?;
        Self::append_es256_coord(&mut raw_signature, signature.s().to_vec())?;

        Ok(format!(
            "{}.{}",
            signing_input,
            general_purpose::URL_SAFE_NO_PAD.encode(raw_signature)
        ))
    }

    fn append_es256_coord(target: &mut Vec<u8>, coord: Vec<u8>) -> Result<(), ArbError> {
        let first_non_zero = coord
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or_else(|| coord.len().saturating_sub(1));
        let coord = &coord[first_non_zero..];
        if coord.len() > 32 {
            return Err(ArbError::SerializationError(
                "Coinbase JWT coordinate is larger than ES256 size".to_string(),
            ));
        }
        target.extend(std::iter::repeat(0).take(32 - coord.len()));
        target.extend_from_slice(coord);
        Ok(())
    }

    fn coinbase_request_path(
        &self,
        endpoint: &str,
        query: Option<&str>,
    ) -> Result<String, ArbError> {
        let base =
            Url::parse(&self.base_url).map_err(|e| ArbError::SerializationError(e.to_string()))?;
        let base_path = base.path().trim_end_matches('/');
        let endpoint_path = endpoint.trim_start_matches('/');
        let mut path = if base_path.is_empty() {
            format!("/{}", endpoint_path)
        } else {
            format!("{}/{}", base_path, endpoint_path)
        };
        if let Some(query) = query
            && !query.is_empty()
        {
            path.push('?');
            path.push_str(query);
        }
        Ok(path)
    }

    fn base_url_host(&self) -> Result<String, ArbError> {
        let base =
            Url::parse(&self.base_url).map_err(|e| ArbError::SerializationError(e.to_string()))?;
        base.host_str()
            .map(str::to_string)
            .ok_or_else(|| ArbError::SerializationError("Exchange base URL missing host".into()))
    }

    fn normalize_symbol(&self, symbol: &str) -> String {
        let mut symbol = match self.exchange_type {
            ExchangeType::Okx | ExchangeType::Coinbase => symbol.replace('/', "-"),
            ExchangeType::Binance | ExchangeType::Bybit | ExchangeType::Kraken => {
                symbol.replace('/', "")
            }
        };
        symbol = symbol.replace("WETH", "ETH").replace("WBTC", "BTC");
        symbol.to_uppercase()
    }

    fn client_order_id(prefix: &str) -> String {
        format!("{}{}", prefix, Self::get_timestamp())
    }

    fn encode_form(params: &[(&str, String)]) -> String {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in params {
            serializer.append_pair(key, value);
        }
        serializer.finish()
    }

    fn okx_timestamp() -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn nonce_hex() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{:032x}", nanos)
    }

    async fn parse_http_response(
        &self,
        resp: reqwest::Response,
    ) -> Result<serde_json::Value, ArbError> {
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        if !status.is_success() {
            log::error!("EXCHANGE API ERROR [{}]: {}", status, text);
            return Err(ArbError::SerializationError(format!(
                "Exchange Error: {}, Body: {}",
                status, text
            )));
        }

        serde_json::from_str(&text)
            .map_err(|e| ArbError::SerializationError(format!("JSON Error: {}", e)))
    }

    fn request_builder(
        &self,
        method: &str,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, ArbError> {
        match method {
            "GET" => Ok(self.http_client.get(url)),
            "POST" => Ok(self.http_client.post(url)),
            "DELETE" => Ok(self.http_client.delete(url)),
            _ => Err(ArbError::InvalidType),
        }
    }

    fn unsupported_streaming(exchange_type: ExchangeType) -> ArbError {
        ArbError::SerializationError(format!(
            "WebSocket order book streaming is not implemented for {:?}",
            exchange_type
        ))
    }

    fn get_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn construct_order_book(
        &self,
        symbol: String,
        bids: Vec<(Decimal, Decimal)>,
        asks: Vec<(Decimal, Decimal)>,
    ) -> OrderBook {
        let best_bid = *bids.first().unwrap_or(&(Decimal::ONE, Decimal::ONE));
        let best_ask = *asks.first().unwrap_or(&(Decimal::ONE, Decimal::ONE));
        let mid = (best_bid.0 + best_ask.0) / Decimal::new(2, 0);
        let spread_bps = if mid.is_zero() {
            Decimal::ZERO
        } else {
            ((best_ask.0 - best_bid.0) * Decimal::new(10000, 0)) / mid
        };

        OrderBook {
            symbol,
            timestamp: Self::get_timestamp(),
            bids,
            asks,
            best_bid,
            best_ask,
            mid_price: mid.normalize(),
            spread_bps: spread_bps.normalize(),
        }
    }

    fn parse_levels(&self, data: &serde_json::Value) -> Result<Vec<(Decimal, Decimal)>, ArbError> {
        let mut levels = Vec::new();
        if let Some(arr) = data.as_array() {
            for item in arr {
                let price_str = item
                    .get(0)
                    .and_then(|value| value.as_str())
                    .or_else(|| item.get("price").and_then(|value| value.as_str()))
                    .ok_or(ArbError::DecimalError)?;
                let qty_str = item
                    .get(1)
                    .and_then(|value| value.as_str())
                    .or_else(|| item.get("size").and_then(|value| value.as_str()))
                    .or_else(|| item.get("qty").and_then(|value| value.as_str()))
                    .ok_or(ArbError::DecimalError)?;
                let price = Decimal::from_str(price_str).map_err(|_| ArbError::DecimalError)?;
                let qty = Decimal::from_str(qty_str).map_err(|_| ArbError::DecimalError)?;
                levels.push((price, qty));
            }
        }
        Ok(levels)
    }

    async fn validate_connection(&self) -> Result<(), ArbError> {
        let endpoint = match self.exchange_type {
            ExchangeType::Binance => "/api/v3/time",
            ExchangeType::Bybit => "/v5/market/time",
            ExchangeType::Okx => "/api/v5/public/time",
            ExchangeType::Coinbase => "/time",
            ExchangeType::Kraken => "/0/public/SystemStatus",
        };
        let url = format!("{}{}", self.base_url, endpoint);
        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ArbError::SerializationError(format!(
                "Status {}",
                resp.status()
            )));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn map_order_response_with_request(
        &self,
        data: serde_json::Value,
        symbol: &str,
        side_hint: Option<String>,
        order_type_hint: Option<String>,
        time_in_force_hint: Option<String>,
        amount_requested_hint: Option<TokenAmount>,
        price_hint: Option<Decimal>,
    ) -> Result<OrderResult, ArbError> {
        let asset = symbol.split('/').next().unwrap_or("").to_string();
        let d = match self.exchange_type {
            ExchangeType::Binance => data.clone(),
            ExchangeType::Bybit => data["result"].clone(),
            ExchangeType::Okx => data["data"].get(0).cloned().unwrap_or_default(),
            ExchangeType::Coinbase => data["success_response"].clone(),
            ExchangeType::Kraken => data["result"].clone(),
        };

        let id = match self.exchange_type {
            ExchangeType::Binance | ExchangeType::Bybit => {
                Self::json_value_to_string(&d["orderId"])
                    .or_else(|| Self::json_value_to_string(&d["order_id"]))
            }
            ExchangeType::Okx => Self::json_value_to_string(&d["ordId"])
                .or_else(|| Self::json_value_to_string(&d["clOrdId"])),
            ExchangeType::Coinbase => Self::json_value_to_string(&d["order_id"]),
            ExchangeType::Kraken => d["txid"]
                .as_array()
                .and_then(|ids| ids.first())
                .and_then(Self::json_value_to_string),
        }
        .unwrap_or_default();

        let response_side = d["side"].as_str().map(str::to_lowercase);
        let side = side_hint
            .or(response_side)
            .unwrap_or_else(|| "unknown".to_string());

        let order_type = order_type_hint.unwrap_or_else(|| {
            d["type"]
                .as_str()
                .or(d["orderType"].as_str())
                .or(d["ordType"].as_str())
                .unwrap_or("limit")
                .to_lowercase()
        });

        let time_in_force = time_in_force_hint.unwrap_or_else(|| {
            d["timeInForce"]
                .as_str()
                .or(d["time_in_force"].as_str())
                .unwrap_or("GTC")
                .to_string()
        });

        let amount_requested = if let Some(amount) = amount_requested_hint {
            amount
        } else {
            TokenAmount::from_human(
                d["origQty"]
                    .as_str()
                    .or(d["qty"].as_str())
                    .or(d["sz"].as_str())
                    .or(d["base_size"].as_str())
                    .unwrap_or("0"),
                8,
                Some(asset.clone()),
            )?
        };

        let amount_filled = TokenAmount::from_human(
            d["executedQty"]
                .as_str()
                .or(d["cumExecQty"].as_str())
                .or(d["accFillSz"].as_str())
                .or(d["filled_size"].as_str())
                .unwrap_or("0"),
            8,
            Some(asset.clone()),
        )?;

        let response_price = d["price"]
            .as_str()
            .or(d["avgPrice"].as_str())
            .or(d["px"].as_str())
            .or(d["average_filled_price"].as_str())
            .and_then(|value| Decimal::from_str(value).ok());
        let avg_fill_price = response_price.or(price_hint).unwrap_or(Decimal::ZERO);

        let status = match self.exchange_type {
            ExchangeType::Okx => {
                if d["sCode"].as_str().unwrap_or("0") == "0" {
                    "accepted".to_string()
                } else {
                    d["sMsg"].as_str().unwrap_or("rejected").to_lowercase()
                }
            }
            ExchangeType::Coinbase => {
                if data["success"].as_bool().unwrap_or(true) {
                    "accepted".to_string()
                } else {
                    data["failure_reason"]
                        .as_str()
                        .unwrap_or("rejected")
                        .to_lowercase()
                }
            }
            ExchangeType::Kraken => {
                if !id.is_empty() {
                    "accepted".to_string()
                } else {
                    "unknown".to_string()
                }
            }
            ExchangeType::Binance | ExchangeType::Bybit => d["status"]
                .as_str()
                .or(d["orderStatus"].as_str())
                .unwrap_or("UNKNOWN")
                .to_lowercase(),
        };

        Ok(OrderResult {
            id,
            symbol: symbol.to_string(),
            side,
            order_type,
            time_in_force,
            amount_requested,
            amount_filled,
            avg_fill_price,
            fee: TokenAmount::from_human("0", 8, None)?,
            status,
            timestamp: Self::get_timestamp(),
        })
    }

    fn json_value_to_string(value: &serde_json::Value) -> Option<String> {
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_u64().map(|value| value.to_string()))
            .or_else(|| value.as_i64().map(|value| value.to_string()))
    }

    pub async fn create_market_order(
        &self,
        symbol: &str,
        side: &str,
        amount: TokenAmount,
    ) -> Result<OrderResult, ArbError> {
        let symbol_clean = self.normalize_symbol(symbol);
        let amount_str = amount.to_human().normalize().to_string();
        let side_upper = side.to_uppercase();
        let side_lower = side.to_lowercase();
        let (endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => (
                "/api/v3/order",
                format!(
                    "symbol={}&side={}&type=MARKET&quantity={}",
                    symbol_clean, side_upper, amount_str
                ),
            ),
            ExchangeType::Bybit => (
                "/v5/order/create",
                serde_json::json!({
                    "category": "spot", "symbol": symbol_clean, "side": side_upper,
                    "orderType": "Market", "qty": amount_str
                })
                .to_string(),
            ),
            ExchangeType::Okx => (
                "/api/v5/trade/order",
                serde_json::json!({
                    "instId": symbol_clean,
                    "tdMode": "cash",
                    "clOrdId": Self::client_order_id("arb"),
                    "side": side_lower,
                    "ordType": "market",
                    "sz": amount_str,
                    "tgtCcy": "base_ccy"
                })
                .to_string(),
            ),
            ExchangeType::Coinbase => (
                "/orders",
                serde_json::json!({
                    "client_order_id": Self::client_order_id("arb"),
                    "product_id": symbol_clean,
                    "side": side_upper,
                    "order_configuration": {
                        "market_market_ioc": {
                            "base_size": amount_str,
                            "rfq_disabled": true
                        }
                    }
                })
                .to_string(),
            ),
            ExchangeType::Kraken => (
                "/0/private/AddOrder",
                Self::encode_form(&[
                    ("pair", symbol_clean),
                    ("type", side_lower),
                    ("ordertype", "market".to_string()),
                    ("volume", amount_str),
                ]),
            ),
        };

        let data = self
            .signed_request("POST", endpoint, &params, RequestPriority::High)
            .await?;
        self.map_order_response_with_request(
            data,
            symbol,
            Some(side.to_lowercase()),
            Some("market".to_string()),
            Some("IOC".to_string()),
            Some(amount),
            None,
        )
    }
}

#[cfg(test)]
mod private_api_tests {
    use super::*;

    fn test_client(exchange_type: ExchangeType, secret: &str) -> ExchangeClient {
        ExchangeClient {
            config: crate::config::ExchangeConfig {
                production: false,
                binance_http_url: "https://example.com".to_string(),
                binance_ws_url: "wss://example.com".to_string(),
                cex_fee_bps: 0.0,
                arbitrum_rpc_url: "https://example.com".to_string(),
                arbitrum_chain_id: 42161,
                pair: "ETH/USDT".to_string(),
                weth_address: "0x0000000000000000000000000000000000000000".to_string(),
                usdc_address: "0x0000000000000000000000000000000000000000".to_string(),
                api_key: "test-key".to_string(),
                secret: secret.to_string(),
                passphrase: Some("test-passphrase".to_string()),
                is_sandbox: true,
                skip_connection_validation: true,
            },
            exchange_type,
            http_client: Client::new(),
            base_url: "https://example.com".to_string(),
            ws_url: "wss://example.com".to_string(),
            rate_limiter: RateLimiter::new(1),
        }
    }

    #[test]
    fn kraken_signature_matches_official_example() {
        let client = test_client(
            ExchangeType::Kraken,
            "kQH5HW/8p1uGOVjbgWA7FunAmGO8lsSUXNsu3eow76sz84Q18fWxnyRzBHCd3pd5nE9qa99HAZtuZuj6F1huXg==",
        );
        let signature = client
            .sign_kraken(
                "/0/private/AddOrder",
                "1616492376594",
                "nonce=1616492376594&ordertype=limit&pair=XBTUSD&price=37500&type=buy&volume=1.25",
            )
            .unwrap();

        assert_eq!(
            signature,
            "4/dpxb3iT4tp/ZCVEwSnEsLxx0bqyhLpdfOpc6fn7OR8+UClSV5n9E6aSS8MPtnRfp32bAb0nmbRn6H8ndwLUQ=="
        );
    }

    #[test]
    fn coinbase_request_path_includes_brokerage_base_path() {
        let mut client = test_client(ExchangeType::Coinbase, "unused");
        client.base_url = "https://api.coinbase.com/api/v3/brokerage".to_string();

        let path = client.coinbase_request_path("/orders", None).unwrap();

        assert_eq!(path, "/api/v3/brokerage/orders");
    }
}
