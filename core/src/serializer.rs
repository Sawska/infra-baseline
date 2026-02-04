use crate::error::ArbError;
use alloy_primitives::{B256, Keccak256};
use serde::Serialize;
use serde_json::ser::Formatter;

/// A custom formatter to ensure no whitespace in JSON
#[derive(Clone, Debug)]
struct CanonicalFormatter;

impl Formatter for CanonicalFormatter {}

pub struct CanonicalSerializer;

impl CanonicalSerializer {
    pub fn serialize<T: Serialize>(obj: &T) -> Result<Vec<u8>, ArbError> {
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, CanonicalFormatter);
        obj.serialize(&mut ser)
            .map_err(|e| ArbError::SerializationError(e.to_string()))?;
        Ok(buf)
    }

    pub fn hash<T: Serialize>(obj: &T) -> Result<B256, ArbError> {
        let bytes = Self::serialize(obj)?;
        let mut hasher = Keccak256::new();
        hasher.update(bytes);
        Ok(hasher.finalize())
    }

    pub fn verify_determinism<T: Serialize>(obj: &T, iterations: usize) -> bool {
        if iterations == 0 {
            return true;
        }
        let first = match Self::serialize(obj) {
            Ok(b) => b,
            Err(_) => return false,
        };
        for _ in 1..iterations {
            let current = match Self::serialize(obj) {
                Ok(b) => b,
                Err(_) => return false,
            };
            if first != current {
                return false;
            }
        }
        true
    }
}
