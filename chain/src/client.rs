use crate::error::ChainError;
use alloy_network::Ethereum;
use alloy_primitives::{Address as AlloyAddress, TxKind, B256, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_types::{
    BlockId, BlockNumberOrTag, TransactionInput, TransactionRequest as AlloyTxRequest,
};
use arb_core::{Address, TokenAmount, TransactionReceipt, TransactionRequest};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GasPrice {
    pub base_fee: u128,
    pub priority_fee_low: u128,
    pub priority_fee_medium: u128,
    pub priority_fee_high: u128,
}

impl GasPrice {
    pub fn get_max_fee(&self, priority: &str, buffer: f64) -> u128 {
        let p_fee = match priority {
            "low" => self.priority_fee_low,
            "high" => self.priority_fee_high,
            _ => self.priority_fee_medium,
        };
        let base_with_buffer = (self.base_fee as f64 * buffer) as u128;
        base_with_buffer + p_fee
    }
}

pub struct ChainClient {
    provider: Arc<RootProvider<Ethereum>>,
}

impl ChainClient {
    pub fn new(rpc_url: &str) -> Self {
        let url = rpc_url.parse().expect("Invalid RPC URL");
        let provider = RootProvider::new_http(url);

        Self {
            provider: Arc::new(provider),
        }
    }

    pub async fn get_balance(&self, address: Address) -> Result<TokenAmount, ChainError> {
        let balance: U256 = self
            .provider
            .get_balance(AlloyAddress::from(*address.0))
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(TokenAmount {
            raw: balance,
            decimals: 18,
            symbol: Some("ETH".to_string()),
        })
    }

    pub async fn get_nonce(&self, address: Address) -> Result<u64, ChainError> {
        let nonce_u64: u64 = self
            .provider
            .get_transaction_count(AlloyAddress::from(*address.0))
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(nonce_u64)
    }

    pub async fn get_gas_price(&self) -> Result<GasPrice, ChainError> {
        let base_fee_u128: u128 = self
            .provider
            .get_gas_price()
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(GasPrice {
            base_fee: base_fee_u128,
            priority_fee_low: 1_000_000_000,
            priority_fee_medium: 2_000_000_000,
            priority_fee_high: 5_000_000_000,
        })
    }

    pub async fn estimate_gas(&self, tx: &TransactionRequest) -> Result<u64, ChainError> {
        let req = AlloyTxRequest {
            to: Some(tx.to.as_alloy_address().into()),
            value: Some(tx.value.raw),
            input: tx.data.clone().into(),
            ..Default::default()
        };

        let gas_u64: u64 = self
            .provider
            .estimate_gas(req)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(gas_u64)
    }

    pub async fn send_transaction(&self, signed_tx: Vec<u8>) -> Result<B256, ChainError> {
        let pending = self
            .provider
            .send_raw_transaction(&signed_tx)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(*pending.tx_hash())
    }

    pub async fn wait_for_receipt(&self, tx_hash: B256) -> Result<TransactionReceipt, ChainError> {
        let timeout = Duration::from_secs(120);
        let interval = Duration::from_secs(2);
        let start = Instant::now();

        loop {
            if start.elapsed() > timeout {
                return Err(ChainError::Timeout);
            }

            let receipt_opt = self
                .provider
                .get_transaction_receipt(tx_hash)
                .await
                .map_err(|e| ChainError::RpcError(e.to_string()))?;

            if let Some(receipt) = receipt_opt {
                return Ok(TransactionReceipt {
                    tx_hash: format!("{:?}", tx_hash),
                    block_number: receipt.block_number.unwrap_or(0),
                    status: receipt.status(),
                    gas_used: receipt.gas_used,
                    effective_gas_price: U256::from(receipt.effective_gas_price),
                    logs: vec![],
                });
            }

            sleep(interval).await;
        }
    }

    pub async fn get_transaction(
        &self,
        tx_hash: B256,
    ) -> Result<Option<serde_json::Value>, ChainError> {
        let tx = self
            .provider
            .get_transaction_by_hash(tx_hash)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(tx.map(|t| serde_json::to_value(t).unwrap_or_default()))
    }

    pub async fn get_receipt(
        &self,
        tx_hash: B256,
    ) -> Result<Option<TransactionReceipt>, ChainError> {
        let receipt = self
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        match receipt {
            Some(r) => Ok(Some(TransactionReceipt {
                tx_hash: format!("{:?}", tx_hash),
                block_number: r.block_number.unwrap_or(0),
                status: r.status(),
                gas_used: r.gas_used,
                effective_gas_price: U256::from(r.effective_gas_price),
                logs: vec![],
            })),
            None => Ok(None),
        }
    }

    pub async fn call(
        &self,
        tx: &TransactionRequest,
        block: Option<BlockNumberOrTag>,
    ) -> Result<Vec<u8>, ChainError> {
        let req = AlloyTxRequest {
            to: Some(TxKind::Call(tx.to.as_alloy_address())),
            value: Some(tx.value.raw),
            input: TransactionInput::new(tx.data.clone()),
            ..Default::default()
        };

        let block_id = match block.unwrap_or(BlockNumberOrTag::Latest) {
            BlockNumberOrTag::Number(n) => BlockId::Number(BlockNumberOrTag::Number(n)),
            BlockNumberOrTag::Latest => BlockId::latest(),
            BlockNumberOrTag::Pending => BlockId::pending(),
            BlockNumberOrTag::Finalized => BlockId::finalized(),
            BlockNumberOrTag::Safe => BlockId::safe(),
            BlockNumberOrTag::Earliest => BlockId::earliest(),
        };

        let result = self
            .provider
            .call(req)
            .block(block_id)
            .await
            .map_err(|e| ChainError::RpcError(e.to_string()))?;

        Ok(result.to_vec())
    }
}

trait ToAlloy {
    fn as_alloy_address(&self) -> AlloyAddress;
}

impl ToAlloy for Address {
    fn as_alloy_address(&self) -> AlloyAddress {
        AlloyAddress::from(*self.0)
    }
}
