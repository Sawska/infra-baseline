#![allow(clippy::collapsible_if)]
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, ProviderBuilder, WsConnect, network::TransactionResponse};
use alloy_rpc_types::{Filter, Transaction, TransactionTrait};
use alloy_sol_types::{SolCall, SolEvent};
use anyhow::Result;
use futures_util::StreamExt;
use rust_decimal::Decimal;
use std::future::Future;

alloy_sol_types::sol! {
    function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] memory path, address to, uint256 deadline);
    function swapExactETHForTokens(uint256 amountOutMin, address[] memory path, address to, uint256 deadline);
    function swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] memory path, address to, uint256 deadline);

    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);

    struct ExactOutputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountOut;
        uint256 amountInMaximum;
        uint160 sqrtPriceLimitX96;
    }
    function exactOutputSingle(ExactOutputSingleParams calldata params) external payable returns (uint256 amountIn);


    event Sync(uint112 reserve0, uint112 reserve1);
    event Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );
}

#[derive(Debug, Clone)]
pub struct ParsedSwap {
    pub tx_hash: B256,
    pub router: Address,
    pub dex: String,
    pub method: String,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub amount_in: U256,
    pub min_amount_out: U256,
    pub deadline: U256,
    pub sender: Address,
    pub gas_price: Option<u128>,
}

impl ParsedSwap {
    /// Calculate implied slippage tolerance.
    /// Note: This returns 0.0 by default as calculating true slippage
    /// requires the real-time spot price/reserves which aren't in the tx data.
    pub fn slippage_tolerance(&self) -> Decimal {
        Decimal::ZERO
    }
}

#[derive(Debug, Clone)]
pub enum MonitorEvent {
    MempoolSwap(Box<ParsedSwap>),
    PoolUpdate {
        pool_address: Address,
        block_number: u64,
    },
}

pub struct MempoolMonitor<F> {
    ws_url: String,
    callback: F,
}

impl<F, Fut> MempoolMonitor<F>
where
    F: Fn(MonitorEvent) -> Fut + Send + std::marker::Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    pub fn new(ws_url: String, callback: F) -> Self {
        Self { ws_url, callback }
    }

    /// Start monitoring pending transactions.
    pub async fn start(&self) -> Result<()> {
        let ws = WsConnect::new(self.ws_url.clone());
        let provider = ProviderBuilder::new().connect_ws(ws).await?;

        let pending_sub = provider.subscribe_pending_transactions().await?;
        let mut pending_stream = pending_sub.into_stream();

        let v2_sub = provider
            .subscribe_logs(&Filter::new().event_signature(Sync::SIGNATURE_HASH))
            .await?;
        let mut v2_stream = v2_sub.into_stream();

        let v3_sub = provider
            .subscribe_logs(&Filter::new().event_signature(Swap::SIGNATURE_HASH))
            .await?;
        let mut v3_stream = v3_sub.into_stream();

        loop {
            tokio::select! {
                Some(tx_hash) = pending_stream.next() => {
                    if let Ok(Some(tx)) = provider.get_transaction_by_hash(tx_hash).await {
                        if let Some(swap) = self.parse_transaction(&tx) {
                            (self.callback)(MonitorEvent::MempoolSwap(Box::new(swap))).await;
                        }
                    }
                }
                Some(log) = v2_stream.next() => {
                    let block_number = log.block_number.unwrap_or(0);
                    (self.callback)(MonitorEvent::PoolUpdate {
                        pool_address: log.address(),
                        block_number,
                    }).await;
                }
                Some(log) = v3_stream.next() => {
                    let block_number = log.block_number.unwrap_or(0);
                    (self.callback)(MonitorEvent::PoolUpdate {
                        pool_address: log.address(),
                        block_number,
                    }).await;
                }
            }
        }
    }

    /// Parse transaction to extract swap details.
    /// Returns None if not a known swap type.
    pub fn parse_transaction(&self, tx: &Transaction) -> Option<ParsedSwap> {
        let input = &tx.input();
        if input.len() < 4 {
            return None;
        }

        let selector = &input[0..4];
        let router = tx.to()?;
        let sender = tx.from();
        let gas_price = tx.inner.gas_price();
        let tx_hash = tx.inner.hash();

        if selector == swapExactTokensForTokensCall::SELECTOR {
            if let Ok(decoded) = swapExactTokensForTokensCall::abi_decode(input) {
                return Some(ParsedSwap {
                    tx_hash: *tx_hash,
                    router,
                    dex: "UniswapV2".to_string(),
                    method: "swapExactTokensForTokens".to_string(),
                    token_in: decoded.path.first().cloned(),
                    token_out: decoded.path.last().cloned(),
                    amount_in: decoded.amountIn,
                    min_amount_out: decoded.amountOutMin,
                    deadline: decoded.deadline,
                    sender,
                    gas_price,
                });
            }
        }

        if selector == swapExactETHForTokensCall::SELECTOR {
            if let Ok(decoded) = swapExactETHForTokensCall::abi_decode(input) {
                return Some(ParsedSwap {
                    tx_hash: *tx_hash,
                    router,
                    dex: "UniswapV2".to_string(),
                    method: "swapExactETHForTokens".to_string(),
                    token_in: None,
                    token_out: decoded.path.last().cloned(),
                    amount_in: tx.inner.value(),
                    min_amount_out: decoded.amountOutMin,
                    deadline: decoded.deadline,
                    sender,
                    gas_price,
                });
            }
        }

        if selector == swapExactTokensForETHCall::SELECTOR {
            if let Ok(decoded) = swapExactTokensForETHCall::abi_decode(input) {
                return Some(ParsedSwap {
                    tx_hash: *tx_hash,
                    router,
                    dex: "UniswapV2".to_string(),
                    method: "swapExactTokensForETH".to_string(),
                    token_in: decoded.path.first().cloned(),
                    token_out: None,
                    amount_in: decoded.amountIn,
                    min_amount_out: decoded.amountOutMin,
                    deadline: decoded.deadline,
                    sender,
                    gas_price,
                });
            }
        }

        if selector == exactInputSingleCall::SELECTOR {
            if let Ok(decoded) = exactInputSingleCall::abi_decode(input) {
                return Some(ParsedSwap {
                    tx_hash: *tx_hash,
                    router,
                    dex: "UniswapV3".to_string(),
                    method: "exactInputSingle".to_string(),
                    token_in: Some(decoded.params.tokenIn),
                    token_out: Some(decoded.params.tokenOut),
                    amount_in: decoded.params.amountIn,
                    min_amount_out: decoded.params.amountOutMinimum,
                    deadline: U256::from(decoded.params.deadline),
                    sender,
                    gas_price,
                });
            }
        }

        if selector == exactOutputSingleCall::SELECTOR {
            if let Ok(decoded) = exactOutputSingleCall::abi_decode(input) {
                return Some(ParsedSwap {
                    tx_hash: *tx_hash,
                    router,
                    dex: "UniswapV3".to_string(),
                    method: "exactOutputSingle".to_string(),
                    token_in: Some(decoded.params.tokenIn),
                    token_out: Some(decoded.params.tokenOut),
                    amount_in: decoded.params.amountInMaximum,
                    min_amount_out: decoded.params.amountOut,
                    deadline: U256::from(decoded.params.deadline),
                    sender,
                    gas_price,
                });
            }
        }

        None
    }
}
