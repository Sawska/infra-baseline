use crate::client::ChainClient;
use crate::error::ChainError;
use alloy_primitives::{B256, Bytes};
use arb_core::{Address, TokenAmount, TransactionReceipt, TransactionRequest, WalletManager};

pub struct TransactionBuilder<'a> {
    client: &'a ChainClient,
    wallet: &'a WalletManager,
    to: Option<Address>,
    value: Option<TokenAmount>,
    data: Bytes,
    nonce: Option<u64>,
    chain_id: u64,
    gas_limit: Option<u64>,
    priority: String,
    gas_estimate_buffer: f64,
    should_estimate_gas: bool,
}

impl<'a> TransactionBuilder<'a> {
    pub fn new(client: &'a ChainClient, wallet: &'a WalletManager) -> Self {
        Self {
            client,
            wallet,
            to: None,
            value: None,
            data: Bytes::new(),
            nonce: None,
            gas_limit: None,
            priority: "medium".to_string(),
            chain_id: 42161,
            gas_estimate_buffer: 1.2,
            should_estimate_gas: false,
        }
    }

    pub fn to(mut self, address: Address) -> Self {
        self.to = Some(address);
        self
    }

    pub fn chain_id(mut self, id: u64) -> Self {
        self.chain_id = id;
        self
    }

    pub fn value(mut self, amount: TokenAmount) -> Self {
        self.value = Some(amount);
        self
    }

    pub fn data(mut self, calldata: Vec<u8>) -> Self {
        self.data = Bytes::from(calldata);
        self
    }

    pub fn nonce(mut self, n: u64) -> Self {
        self.nonce = Some(n);
        self
    }

    pub fn gas_limit(mut self, limit: u64) -> Self {
        self.gas_limit = Some(limit);
        self
    }

    pub fn with_gas_estimate(mut self, buffer: f64) -> Self {
        self.should_estimate_gas = true;
        self.gas_estimate_buffer = buffer;
        self
    }

    pub fn with_gas_price(mut self, priority: &str) -> Self {
        self.priority = priority.to_string();
        self
    }

    pub async fn build(&self) -> Result<TransactionRequest, ChainError> {
        let to = self
            .to
            .ok_or_else(|| ChainError::Internal("Target address missing".into()))?;

        let value = self.value.clone().unwrap_or_else(|| {
            TokenAmount::from_human("0", 18, None).expect("hardcoded zero amount must be valid")
        });

        let nonce = match self.nonce {
            Some(n) => n,
            None => self.client.get_nonce(self.wallet.address()).await?,
        };

        let gas_price_info = self.client.get_gas_price().await?;
        let max_fee = gas_price_info.get_max_fee(&self.priority, 1.2);

        let mut tx = TransactionRequest {
            to,
            from: Some(self.wallet.address()),
            value,
            data: self.data.clone(),
            nonce: Some(nonce),
            gas_limit: self.gas_limit,
            max_fee_per_gas: Some(max_fee),
            max_priority_fee: Some(gas_price_info.priority_fee_medium),
            chain_id: self.chain_id,
        };

        if self.should_estimate_gas && tx.gas_limit.is_none() {
            let estimated = self.client.estimate_gas(&tx).await?;
            tx.gas_limit = Some((estimated as f64 * self.gas_estimate_buffer) as u64);
        }

        Ok(tx)
    }

    pub async fn build_and_sign(&self) -> Result<Vec<u8>, ChainError> {
        let tx = self.build().await?;
        let signature = self
            .wallet
            .sign_transaction(&tx)
            .map_err(|e| ChainError::Internal(e.to_string()))?;
        Ok(tx.encode_signed(&signature))
    }

    pub async fn send(&self) -> Result<B256, ChainError> {
        let signed_tx = self.build_and_sign().await?;
        self.client.send_transaction(signed_tx).await
    }

    pub async fn send_and_wait(&self) -> Result<TransactionReceipt, ChainError> {
        let tx_hash = self.send().await?;
        self.client.wait_for_receipt(tx_hash).await
    }
}
