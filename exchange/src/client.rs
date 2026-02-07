use arb_core::error::ArbError;
use arb_core::types::TokenAmount;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::Client;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

type HmacSha256 = Hmac<Sha256>;

/// A smart, weight-aware token bucket rate limiter.
/// It refills tokens based on time and allows bursts for execution.
pub struct RateLimiter {
    capacity: f64,
    pub tokens: Arc<Mutex<f64>>,
    last_fill: Arc<Mutex<Instant>>,
    pub tokens_per_ms: f64,
}

impl RateLimiter {
    /// Creates a new limiter.
    /// `max_weight_per_minute` is the exchange's stated limit (e.g., 1200 for Binance).
    pub fn new(max_weight_per_minute: u64) -> Self {
        let capacity = max_weight_per_minute as f64;
        Self {
            capacity,
            tokens: Arc::new(Mutex::new(capacity)),
            last_fill: Arc::new(Mutex::new(Instant::now())),
            tokens_per_ms: capacity / 60_000.0,
        }
    }

    /// Explicitly updates the bucket based on exchange response headers.
    /// This prevents our local state from drifting away from the exchange's server-side state.
    pub async fn update_from_headers(&self, used_weight: f64) {
        let mut tokens = self.tokens.lock().await;
        // Remaining tokens is Capacity - Used
        let remaining = (self.capacity - used_weight).max(0.0);
        *tokens = remaining;
    }

    /// Waits until enough weight capacity is available.
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
                if config.is_sandbox {
                    "https://testnet.binance.vision"
                } else {
                    "https://api.binance.com"
                }
                .to_string(),
                "wss://stream.binance.com:9443/ws".to_string(),
                1200,
            ),
            ExchangeType::Bybit => (
                if config.is_sandbox {
                    "https://api-testnet.bybit.com"
                } else {
                    "https://api.bybit.com"
                }
                .to_string(),
                if config.is_sandbox {
                    "wss://stream-testnet.bybit.com/v5/public/spot"
                } else {
                    "wss://stream.bybit.com/v5/public/spot"
                }
                .to_string(),
                600,
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

        exchange.validate_connection().await?;
        Ok(exchange)
    }

    /// Fetch weight for specific operations to avoid hardcoding everywhere
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
        };

        let mut rb = match method {
            "GET" => self.http_client.get(&url),
            "POST" => self.http_client.post(&url),
            "DELETE" => self.http_client.delete(&url),
            _ => return Err(ArbError::InvalidType),
        };

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

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        if !status.is_success() {
            return Err(ArbError::SerializationError(format!(
                "Exchange Error: {}, Body: {}",
                status, text
            )));
        }

        serde_json::from_str(&text)
            .map_err(|e| ArbError::SerializationError(format!("JSON Error: {}", e)))
    }

    pub async fn fetch_balance(&self) -> Result<HashMap<String, Balance>, ArbError> {
        let (endpoint, key, params) = match self.exchange_type {
            ExchangeType::Binance => ("/api/v3/account", "balances", "".to_string()),
            ExchangeType::Bybit => (
                "/v5/account/wallet-balance",
                "list",
                "accountType=UNIFIED".to_string(),
            ),
        };

        let data = self
            .signed_request("GET", endpoint, &params, RequestPriority::Low)
            .await?;
        let mut balances = HashMap::new();

        let assets = if self.exchange_type == ExchangeType::Binance {
            data[key].as_array()
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
        Ok(balances)
    }

    pub async fn create_limit_ioc_order(
        &self,
        symbol: &str,
        side: &str,
        amount: TokenAmount,
        price: Decimal,
    ) -> Result<OrderResult, ArbError> {
        let symbol_clean = symbol.replace("/", "");
        let (endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => (
                "/api/v3/order",
                format!("symbol={}&side={}&type=LIMIT&timeInForce=IOC&quantity={}&price={}",
                symbol_clean, side.to_uppercase(), amount.to_human(), price)
            ),
            ExchangeType::Bybit => (
                "/v5/order/create",
                serde_json::json!({
                    "category": "spot", "symbol": symbol_clean, "side": side.to_uppercase(),
                    "orderType": "Limit", "qty": amount.to_human(), "price": price.to_string(), "timeInForce": "IOC"
                }).to_string()
            ),
        };

        let data = self
            .signed_request("POST", endpoint, &params, RequestPriority::High)
            .await?;
        self.map_order_response(data, symbol)
    }

    pub async fn fetch_order_book(&self, symbol: &str, limit: u32) -> Result<OrderBook, ArbError> {
        let symbol_clean = symbol.replace("/", "");

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
            data = data[path].clone();
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
        let symbol_clean = symbol.replace("/", "");
        let (method, endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => ("DELETE", "/api/v3/order", format!("symbol={}&orderId={}", symbol_clean, order_id)),
            ExchangeType::Bybit => ("POST", "/v5/order/cancel", serde_json::json!({"category": "spot", "symbol": symbol_clean, "orderId": order_id}).to_string()),
        };

        let data = self
            .signed_request(method, endpoint, &params, RequestPriority::High)
            .await?;
        Ok(match self.exchange_type {
            ExchangeType::Binance => {
                data["orderId"].as_str().is_some() || data["orderId"].as_u64().is_some()
            }
            ExchangeType::Bybit => data["retCode"].as_i64().unwrap_or(-1) == 0,
        })
    }

    fn sign_query(&self, payload: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(self.config.secret.as_bytes()).expect("HMAC Error");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
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
        let best_bid = *bids.first().unwrap_or(&(Decimal::ZERO, Decimal::ZERO));
        let best_ask = *asks.first().unwrap_or(&(Decimal::ZERO, Decimal::ZERO));
        let mid = (best_bid.0 + best_ask.0) / Decimal::new(2, 0);
        let spread_bps = if mid.is_zero() {
            Decimal::ZERO
        } else {
            ((best_ask.0 - best_bid.0) / mid) * Decimal::new(10000, 0)
        };

        OrderBook {
            symbol,
            timestamp: Self::get_timestamp(),
            bids,
            asks,
            best_bid,
            best_ask,
            mid_price: mid.normalize(),
            spread_bps: spread_bps.round_dp(2),
        }
    }

    fn parse_levels(&self, data: &serde_json::Value) -> Result<Vec<(Decimal, Decimal)>, ArbError> {
        let mut levels = Vec::new();
        if let Some(arr) = data.as_array() {
            for item in arr {
                let price = Decimal::from_str(item[0].as_str().ok_or(ArbError::DecimalError)?)
                    .map_err(|_| ArbError::DecimalError)?;
                let qty = Decimal::from_str(item[1].as_str().ok_or(ArbError::DecimalError)?)
                    .map_err(|_| ArbError::DecimalError)?;
                levels.push((price, qty));
            }
        }
        Ok(levels)
    }

    async fn validate_connection(&self) -> Result<(), ArbError> {
        let endpoint = match self.exchange_type {
            ExchangeType::Binance => "/api/v3/time",
            ExchangeType::Bybit => "/v5/market/time",
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

    fn map_order_response(
        &self,
        data: serde_json::Value,
        symbol: &str,
    ) -> Result<OrderResult, ArbError> {
        let asset = symbol.split('/').next().unwrap_or("").to_string();
        let d = if self.exchange_type == ExchangeType::Binance {
            data
        } else {
            data["result"].clone()
        };

        Ok(OrderResult {
            id: d["orderId"].as_str().unwrap_or("").to_string(),
            symbol: symbol.to_string(),
            side: d["side"].as_str().unwrap_or("UNKNOWN").to_lowercase(),
            order_type: d["type"]
                .as_str()
                .or(d["orderType"].as_str())
                .unwrap_or("limit")
                .to_lowercase(),
            time_in_force: d["timeInForce"].as_str().unwrap_or("GTC").to_string(),
            amount_requested: TokenAmount::from_human(
                d["origQty"].as_str().or(d["qty"].as_str()).unwrap_or("0"),
                8,
                Some(asset.clone()),
            )?,
            amount_filled: TokenAmount::from_human(
                d["executedQty"]
                    .as_str()
                    .or(d["cumExecQty"].as_str())
                    .unwrap_or("0"),
                8,
                Some(asset),
            )?,
            avg_fill_price: Decimal::from_str(
                d["price"]
                    .as_str()
                    .or(d["avgPrice"].as_str())
                    .unwrap_or("0"),
            )
            .unwrap_or(Decimal::ZERO),
            fee: TokenAmount::from_human("0", 8, None)?,
            status: d["status"]
                .as_str()
                .or(d["orderStatus"].as_str())
                .unwrap_or("UNKNOWN")
                .to_lowercase(),
            timestamp: Self::get_timestamp(),
        })
    }
}
