use crate::error::ArbError;
use alloy_consensus::SignableTransaction;
use alloy_consensus::{Signed, TxEip1559};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address as AlloyAddress, B256, Bytes, U256};
use alloy_signer::Signature;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
pub struct Address(pub AlloyAddress);

impl Address {
    pub const ZERO: Self = Self(alloy_primitives::Address::ZERO);

    pub fn from_string(s: &str) -> Result<Self, ArbError> {
        s.parse::<AlloyAddress>()
            .map(Address)
            .map_err(|_| ArbError::InvalidAddress)
    }

    pub fn checksum(&self) -> String {
        self.0.to_checksum(None)
    }

    pub fn lower(&self) -> String {
        format!("{:?}", self.0).to_lowercase()
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.checksum())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenAmount {
    pub raw: U256,
    pub decimals: u8,
    pub symbol: Option<String>,
}

impl TokenAmount {
    pub fn from_human(
        amount: &str,
        decimals: u8,
        symbol: Option<String>,
    ) -> Result<Self, ArbError> {
        let dec = Decimal::from_str(amount).map_err(|_| ArbError::DecimalError)?;
        let multiplier = Decimal::from(10u64.pow(decimals as u32));
        let raw_dec = (dec * multiplier).normalize();

        if raw_dec.scale() > 0 {
            return Err(ArbError::DecimalError);
        }

        let raw_str = raw_dec.to_string();
        let raw = U256::from_str_radix(&raw_str, 10).map_err(|_| ArbError::DecimalError)?;

        Ok(Self {
            raw,
            decimals,
            symbol,
        })
    }

    pub fn to_human(&self) -> Decimal {
        let raw_str = self.raw.to_string();
        let mut dec = Decimal::from_str(&raw_str).unwrap_or(Decimal::ZERO);
        dec.set_scale(self.decimals as u32).unwrap();
        dec.normalize()
    }
}

impl std::ops::Add for TokenAmount {
    type Output = Result<Self, ArbError>;

    fn add(self, rhs: Self) -> Self::Output {
        if self.decimals != rhs.decimals {
            return Err(ArbError::MismatchedDecimals(self.decimals, rhs.decimals));
        }
        Ok(Self {
            raw: self.raw + rhs.raw,
            decimals: self.decimals,
            symbol: self.symbol.clone().or(rhs.symbol),
        })
    }
}

impl std::ops::Mul<u64> for TokenAmount {
    type Output = Self;

    fn mul(self, rhs: u64) -> Self::Output {
        Self {
            raw: self.raw * U256::from(rhs),
            decimals: self.decimals,
            symbol: self.symbol,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRequest {
    pub to: Address,
    pub value: TokenAmount,
    pub data: Bytes,
    pub nonce: Option<u64>,
    pub gas_limit: Option<u64>,
    pub max_fee_per_gas: Option<u128>,
    pub max_priority_fee: Option<u128>,
    pub chain_id: u64,
    pub from: Option<Address>,
}

impl TransactionRequest {
    pub fn signing_hash(&self) -> B256 {
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce: self.nonce.unwrap_or(0),
            max_priority_fee_per_gas: self.max_priority_fee.unwrap_or(0),
            max_fee_per_gas: self.max_fee_per_gas.unwrap_or(0),
            gas_limit: self.gas_limit.unwrap_or(21000),
            to: alloy_primitives::TxKind::Call(self.to.0),
            value: self.value.raw,
            input: self.data.clone(),
            access_list: Default::default(),
        };

        tx.signature_hash()
    }

    pub fn encode_signed(&self, signature: &Signature) -> Vec<u8> {
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce: self.nonce.unwrap_or(0),
            max_priority_fee_per_gas: self.max_priority_fee.unwrap_or(0),
            max_fee_per_gas: self.max_fee_per_gas.unwrap_or(0),
            gas_limit: self.gas_limit.unwrap_or(21_000),
            to: alloy_primitives::TxKind::Call(self.to.0),
            value: self.value.raw,
            input: self.data.clone(),
            access_list: Default::default(),
        };

        let signed_tx = Signed::new_unchecked(tx, *signature, self.signing_hash());

        let mut out = Vec::new();
        signed_tx.encode_2718(&mut out);

        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReceipt {
    pub tx_hash: String,
    pub block_number: u64,
    pub status: bool,
    pub gas_used: u64,
    pub effective_gas_price: U256,
    pub logs: Vec<serde_json::Value>,
}

impl TransactionReceipt {
    pub fn tx_fee(&self) -> TokenAmount {
        TokenAmount {
            raw: self.effective_gas_price * U256::from(self.gas_used),
            decimals: 18,
            symbol: Some("ETH".to_string()),
        }
    }
}
