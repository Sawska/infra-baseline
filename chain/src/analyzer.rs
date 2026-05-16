use crate::ChainClient;
use alloy_primitives::{Address as AlloyAddress, B256, U256};
use alloy_rpc_types::{Transaction, TransactionTrait};
use alloy_sol_types::{SolCall, SolEvent, SolInterface, sol};
use anyhow::{Context, Result};
use arb_core::{Address, TokenAmount, TransactionReceipt};
use serde::Serialize;
use std::collections::HashMap;

sol! {
    #[derive(Debug, PartialEq, Serialize)]
    interface IERC20 {
        event Transfer(address indexed from, address indexed to, uint256 value);
        function symbol() external view returns (string memory);
        function decimals() external view returns (uint8);
    }

    #[derive(Debug, PartialEq, Serialize)]
    interface IUniswapV3Router {
        function multicall(bytes[] calldata data) external;
    }

    #[derive(Debug, PartialEq, Serialize)]
    interface IUniswapV2Router {
        function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external;
        function swapTokensForExactTokens(uint256 amountOut, uint256 amountInMax, address[] calldata path, address to, uint256 deadline) external;
    }

    #[derive(Serialize)]
    event Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to);

    #[derive(Serialize)]
    event Sync(uint112 reserve0, uint112 reserve1);
}

#[derive(Serialize, Debug)]
pub struct AnalysisReport {
    pub hash: B256,
    pub status: String,
    pub block_number: u64,
    pub from: AlloyAddress,
    pub to: Option<AlloyAddress>,
    pub gas: GasReport,
    pub decoded_call: Option<String>,
    pub events: Vec<DecodedEvent>,
}

#[derive(Serialize, Debug)]
pub struct GasReport {
    pub gas_used: u64,
    pub fee_eth: String,
}

#[derive(Serialize, Debug)]
pub enum DecodedEvent {
    Transfer {
        token: AlloyAddress,
        symbol: String,
        amount: String,
        from: AlloyAddress,
        to: AlloyAddress,
    },
    Swap {
        pool: AlloyAddress,
        to: AlloyAddress,
    },
    Sync {
        pool: AlloyAddress,
        reserve0: U256,
        reserve1: U256,
    },
    Unknown {
        address: AlloyAddress,
        topic0: B256,
    },
}

pub struct Analyzer {
    client: ChainClient,
    token_cache: HashMap<AlloyAddress, (String, u8)>,
    chain_id: u64,
}

impl Analyzer {
    pub fn new(rpc: &str, chain_id: u64) -> Self {
        Self {
            client: ChainClient::new(rpc),
            token_cache: HashMap::new(),
            chain_id,
        }
    }

    async fn get_token_metadata(&mut self, addr: AlloyAddress) -> (String, u8) {
        if let Some(cached) = self.token_cache.get(&addr) {
            return cached.clone();
        }

        let symbol_data = IERC20::symbolCall {}.abi_encode();
        let decimals_data = IERC20::decimalsCall {}.abi_encode();

        let mut req = arb_core::TransactionRequest {
            to: Address(addr),
            from: None,
            value: TokenAmount {
                raw: U256::ZERO,
                decimals: 18,
                symbol: None,
            },
            data: symbol_data.into(),
            nonce: None,
            gas_limit: None,
            max_fee_per_gas: None,
            max_priority_fee: None,
            chain_id: self.chain_id,
        };

        let symbol = self
            .client
            .call(&req, None)
            .await
            .ok()
            .and_then(|res| IERC20::symbolCall::abi_decode_returns(&res).ok())
            .unwrap_or_else(|| {
                format!(
                    "0x{:x}...",
                    u64::from_be_bytes(addr[0..8].try_into().unwrap_or([0; 8]))
                )
            });

        req.data = decimals_data.into();
        let decimals = self
            .client
            .call(&req, None)
            .await
            .ok()
            .and_then(|res| IERC20::decimalsCall::abi_decode_returns(&res).ok())
            .unwrap_or(18);

        self.token_cache.insert(addr, (symbol.clone(), decimals));
        (symbol, decimals)
    }

    pub async fn analyze(&mut self, tx_hash: B256) -> Result<AnalysisReport> {
        let (tx_res, receipt_res) = tokio::join!(
            self.client.get_transaction(tx_hash),
            self.client.get_receipt(tx_hash)
        );

        let tx_val = tx_res?.context("Transaction not found on chain")?;
        let receipt = receipt_res?.context("Receipt not found on chain")?;

        let tx: Transaction =
            serde_json::from_value(tx_val).context("Failed to deserialize transaction")?;

        let report = AnalysisReport {
            hash: tx_hash,
            status: if receipt.status {
                "SUCCESS".to_string()
            } else {
                "FAILED".to_string()
            },
            block_number: receipt.block_number,
            from: tx.inner.signer(),
            to: tx.inner.to(),
            gas: GasReport {
                gas_used: receipt.gas_used,
                fee_eth: receipt.tx_fee().to_human().to_string(),
            },
            decoded_call: self.decode_input(tx.inner.input()),
            events: self.parse_events(&receipt).await?,
        };

        Ok(report)
    }

    fn decode_input(&self, data: &[u8]) -> Option<String> {
        if data.len() < 4 {
            return None;
        }

        if let Ok(call) = IUniswapV3Router::IUniswapV3RouterCalls::abi_decode(data) {
            Some(format!("Uniswap V3: {:?}", call))
        } else if let Ok(call) = IUniswapV2Router::IUniswapV2RouterCalls::abi_decode(data) {
            Some(format!("Uniswap V2: {:?}", call))
        } else if let Ok(call) = IERC20::IERC20Calls::abi_decode(data) {
            Some(format!("ERC-20: {:?}", call))
        } else {
            Some(format!(
                "Unknown (Selector: 0x{})",
                hex::encode(&data[0..4])
            ))
        }
    }

    async fn parse_events(&mut self, receipt: &TransactionReceipt) -> Result<Vec<DecodedEvent>> {
        let mut events = Vec::new();

        for log_val in &receipt.logs {
            let raw_log: alloy_rpc_types::Log = serde_json::from_value(log_val.clone())?;
            let addr = raw_log.address();

            if let Ok(t) = IERC20::Transfer::decode_log(&raw_log.inner) {
                let (sym, dec) = self.get_token_metadata(addr).await;
                let amount = TokenAmount {
                    raw: t.value,
                    decimals: dec,
                    symbol: Some(sym.clone()),
                };
                events.push(DecodedEvent::Transfer {
                    token: addr,
                    symbol: sym,
                    amount: amount.to_human().to_string(),
                    from: t.from,
                    to: t.to,
                });
            } else if let Ok(s) = Swap::decode_log(&raw_log.inner) {
                events.push(DecodedEvent::Swap {
                    pool: addr,
                    to: s.to,
                });
            } else if let Ok(sy) = Sync::decode_log(&raw_log.inner) {
                events.push(DecodedEvent::Sync {
                    pool: addr,
                    reserve0: U256::from(sy.reserve0),
                    reserve1: U256::from(sy.reserve1),
                });
            } else {
                events.push(DecodedEvent::Unknown {
                    address: addr,
                    topic0: raw_log.inner.topics().first().cloned().unwrap_or_default(),
                });
            }
        }
        Ok(events)
    }
}
