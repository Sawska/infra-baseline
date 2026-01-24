use thiserror::Error;

#[derive(Error, Debug)]
pub enum ArbError {
    #[error("Invalid address format")]
    InvalidAddress,
    #[error("Decimal conversion error")]
    DecimalError,
    #[error("Mismatched decimals for operation: {0} vs {1}")]
    MismatchedDecimals(u8, u8),
    #[error("Missing environment variable: {0}")]
    EnvVarMissing(String),
    #[error("Empty message cannot be signed")]
    EmptyMessage,
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("Crypto error: {0}")]
    CryptoError(String),
    #[error("Invalid type for operation")]
    InvalidType,
}
