use arb_core::error::ArbError;
use arb_core::types::TokenAmount;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::Client;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

type HmacSha256 = Hmac<Sha256>;

/// Simple token bucket rate limiter for API compliance.
pub struct RateLimiter {
    last_request: Arc<Mutex<Instant>>,
    min_interval: Duration,
}

impl RateLimiter {
    pub fn new(requests_per_second: u64) -> Self {
        Self {
            last_request: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))),
            min_interval: Duration::from_micros(1_000_000 / requests_per_second),
        }
    }

    pub async fn wait(&self) {
        let mut last = self.last_request.lock().await;
        let now = Instant::now();
        let diff = now.duration_since(*last);

        if diff < self.min_interval {
            let wait_time = self.min_interval - diff;
            sleep(wait_time).await;
            *last += self.min_interval;
        } else {
            *last = now;
        }
    }
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

#[derive(Debug, Clone, Copy)]
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
    pub used_weight: Arc<Mutex<u32>>,
}

impl ExchangeClient {
    pub async fn new(
        config: crate::config::ExchangeConfig,
        exchange_type: ExchangeType,
    ) -> Result<Self, ArbError> {
        let (base_url, ws_url) = match exchange_type {
            ExchangeType::Binance => {
                if config.is_sandbox {
                    (
                        "https://testnet.binance.vision".to_string(),
                        "wss://testnet.binance.vision/ws".to_string(),
                    )
                } else {
                    (
                        "https://api.binance.com".to_string(),
                        "wss://stream.binance.com:9443/ws".to_string(),
                    )
                }
            }
            ExchangeType::Bybit => {
                if config.is_sandbox {
                    (
                        "https://api-testnet.bybit.com".to_string(),
                        "wss://stream-testnet.bybit.com/v5/public/spot".to_string(),
                    )
                } else {
                    (
                        "https://api.bybit.com".to_string(),
                        "wss://stream.bybit.com/v5/public/spot".to_string(),
                    )
                }
            }
        };

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        let rate_limiter = RateLimiter::new(15);

        let exchange = Self {
            config,
            exchange_type,
            http_client: client,
            base_url,
            ws_url,
            rate_limiter,
            used_weight: Arc::new(Mutex::new(0)),
        };

        exchange.validate_connection().await?;
        Ok(exchange)
    }

    /// WebSocket-based order book stream
    pub async fn stream_order_book<F>(&self, symbol: &str, mut callback: F) -> Result<(), ArbError>
    where
        F: FnMut(OrderBook) + Send + 'static,
    {
        let symbol_clean = symbol.replace("/", "").to_lowercase();
        let ws_endpoint = format!("{}/{}@depth20@100ms", self.ws_url, symbol_clean);

        let (ws_stream, _) = connect_async(ws_endpoint)
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        let (_, mut read) = ws_stream.split();

        while let Some(message) = read.next().await {
            if let Ok(Message::Text(text)) = message
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&text)
            {
                let bids = self.parse_levels(&data["bids"])?;
                let asks = self.parse_levels(&data["asks"])?;

                if !bids.is_empty() && !asks.is_empty() {
                    let best_bid = bids[0];
                    let best_ask = asks[0];
                    let mid = (best_bid.0 + best_ask.0) / Decimal::new(2, 0);
                    let spread_bps = ((best_ask.0 - best_bid.0) / mid) * Decimal::new(10000, 0);

                    callback(OrderBook {
                        symbol: symbol.to_string(),
                        timestamp: Self::get_timestamp(),
                        bids,
                        asks,
                        best_bid,
                        best_ask,
                        mid_price: mid.normalize(),
                        spread_bps: spread_bps.round_dp(2),
                    });
                }
            }
        }
        Ok(())
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

    pub async fn fetch_order_book(&self, symbol: &str, limit: u32) -> Result<OrderBook, ArbError> {
        match self.exchange_type {
            ExchangeType::Binance => self.fetch_binance_depth(symbol, limit).await,
            ExchangeType::Bybit => self.fetch_bybit_depth(symbol, limit).await,
        }
    }

    async fn fetch_binance_depth(&self, symbol: &str, limit: u32) -> Result<OrderBook, ArbError> {
        let symbol_clean = symbol.replace("/", "");
        let url = format!(
            "{}/api/v3/depth?symbol={}&limit={}",
            self.base_url, symbol_clean, limit
        );
        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        self.parse_order_book_data(data, symbol, "bids", "asks")
    }

    async fn fetch_bybit_depth(&self, symbol: &str, limit: u32) -> Result<OrderBook, ArbError> {
        let symbol_clean = symbol.replace("/", "");
        let url = format!(
            "{}/v5/market/orderbook?category=spot&symbol={}&limit={}",
            self.base_url, symbol_clean, limit
        );
        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        self.parse_order_book_data(data["result"].clone(), symbol, "b", "a")
    }

    fn parse_order_book_data(
        &self,
        data: serde_json::Value,
        symbol: &str,
        bid_key: &str,
        ask_key: &str,
    ) -> Result<OrderBook, ArbError> {
        let bids = self.parse_levels(&data[bid_key])?;
        let asks = self.parse_levels(&data[ask_key])?;

        if bids.is_empty() || asks.is_empty() {
            return Err(ArbError::InvalidType);
        }

        let best_bid = bids[0];
        let best_ask = asks[0];
        let mid = (best_bid.0 + best_ask.0) / Decimal::new(2, 0);
        let spread_bps = if mid.is_zero() {
            Decimal::ZERO
        } else {
            ((best_ask.0 - best_bid.0) / mid) * Decimal::new(10000, 0)
        };

        Ok(OrderBook {
            symbol: symbol.to_string(),
            timestamp: Self::get_timestamp(),
            bids,
            asks,
            best_bid,
            best_ask,
            mid_price: mid.normalize(),
            spread_bps: spread_bps.round_dp(2),
        })
    }

    pub async fn fetch_balance(&self) -> Result<HashMap<String, Balance>, ArbError> {
        let (endpoint, key) = match self.exchange_type {
            ExchangeType::Binance => ("/api/v3/account", "balances"),
            ExchangeType::Bybit => ("/v5/account/wallet-balance?accountType=SPOT", "list"),
        };

        let data = self.signed_request("GET", endpoint, "").await?;
        let mut balances = HashMap::new();

        let assets = if self.exchange_type as u8 == ExchangeType::Binance as u8 {
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
                    .unwrap_or("0");
                let locked_str = asset["locked"]
                    .as_str()
                    .or(asset["locked"].as_str())
                    .unwrap_or("0");

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
        let params = match self.exchange_type {
            ExchangeType::Binance => format!(
                "symbol={}&side={}&type=LIMIT&timeInForce=IOC&quantity={}&price={}",
                symbol_clean,
                side.to_uppercase(),
                amount.to_human(),
                price
            ),
            ExchangeType::Bybit => format!(
                "category=spot&symbol={}&side={}&orderType=Limit&qty={}&price={}&timeInForce=IOC",
                symbol_clean,
                side.to_uppercase(),
                amount.to_human(),
                price
            ),
        };

        let endpoint = match self.exchange_type {
            ExchangeType::Binance => "/api/v3/order",
            ExchangeType::Bybit => "/v5/order/create",
        };

        let data = self.signed_request("POST", endpoint, &params).await?;
        self.map_order_response(data, symbol)
    }

    async fn signed_request(
        &self,
        method: &str,
        endpoint: &str,
        params: &str,
    ) -> Result<serde_json::Value, ArbError> {
        self.rate_limiter.wait().await;
        let timestamp = Self::get_timestamp();

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
                let recv_window = "5000";
                let sign_payload = format!(
                    "{}{}{}{}",
                    timestamp, self.config.api_key, recv_window, params
                );
                let signature = self.sign_query(&sign_payload);
                let url = format!("{}{}", self.base_url, endpoint);
                (url, Some(params.to_string()), Some(signature))
            }
        };

        let mut rb = match method {
            "GET" => self.http_client.get(&url),
            "POST" => self.http_client.post(&url),
            "DELETE" => self.http_client.delete(&url),
            _ => return Err(ArbError::InvalidType),
        };

        if let ExchangeType::Binance = self.exchange_type {
            rb = rb.header("X-MBX-APIKEY", &self.config.api_key);
        } else {
            rb = rb
                .header("X-BAPI-API-KEY", &self.config.api_key)
                .header("X-BAPI-SIGN", signature.unwrap_or_default())
                .header("X-BAPI-TIMESTAMP", timestamp.to_string())
                .header("X-BAPI-RECV-WINDOW", "5000");
        }

        if let Some(b) = body {
            rb = rb.body(b);
        }

        let resp = rb
            .send()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;

        Ok(data)
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

    fn map_order_response(
        &self,
        data: serde_json::Value,
        symbol: &str,
    ) -> Result<OrderResult, ArbError> {
        let asset = symbol.split('/').next().unwrap_or("").to_string();
        let d = if self.exchange_type as u8 == ExchangeType::Binance as u8 {
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

    pub async fn cancel_order(&self, symbol: &str, order_id: &str) -> Result<bool, ArbError> {
        let symbol_clean = symbol.replace("/", "");

        let (method, endpoint, params) = match self.exchange_type {
            ExchangeType::Binance => (
                "DELETE",
                "/api/v3/order",
                format!("symbol={}&orderId={}", symbol_clean, order_id),
            ),
            ExchangeType::Bybit => (
                "POST",
                "/v5/order/cancel",
                serde_json::json!({
                    "category": "spot",
                    "symbol": symbol_clean,
                    "orderId": order_id
                })
                .to_string(),
            ),
        };

        let data = self.signed_request(method, endpoint, &params).await?;

        let success = match self.exchange_type {
            ExchangeType::Binance => {
                data["orderId"].as_str().is_some() || data["orderId"].as_u64().is_some()
            }
            ExchangeType::Bybit => data["retCode"].as_i64().unwrap_or(-1) == 0,
        };

        Ok(success)
    }
}
