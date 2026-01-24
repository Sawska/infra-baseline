use arb_core::TransactionReceipt;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChainError {
    #[error("RPC request failed: {0}")]
    RpcError(String),

    #[error("Transaction {tx_hash} reverted")]
    TransactionFailed {
        tx_hash: String,
        receipt: TransactionReceipt,
    },

    #[error("Insufficient funds for transaction")]
    InsufficientFunds,

    #[error("Nonce too low")]
    NonceTooLow,

    #[error("Replacement transaction underpriced")]
    ReplacementUnderpriced,

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Timeout waiting for receipt")]
    Timeout,
}
