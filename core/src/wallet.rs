use crate::error::ArbError;
use crate::types::{Address, TransactionRequest};
use alloy_primitives::{B256, Signature};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{Eip712Domain, SolStruct};
use std::env;
use std::fmt;
use std::path::Path;

pub struct WalletManager {
    signer: PrivateKeySigner,
}

impl WalletManager {
    pub fn from_env(env_var: &str) -> Result<Self, ArbError> {
        let pkey = env::var(env_var).map_err(|_| ArbError::EnvVarMissing(env_var.to_string()))?;
        let signer = pkey
            .parse::<PrivateKeySigner>()
            .map_err(|e| ArbError::CryptoError(e.to_string()))?;
        Ok(Self { signer })
    }

    /// Generate a new random wallet. Returns the manager and the hex private key.
    pub fn generate() -> (Self, String) {
        let signer = PrivateKeySigner::random();
        let pkey_hex = hex::encode(signer.to_bytes());
        (Self { signer }, pkey_hex)
    }

    /// Load from an encrypted JSON keyfile (Geth/Clef format).
    pub fn from_keyfile<P: AsRef<Path>>(path: P, password: &str) -> Result<Self, ArbError> {
        let signer = PrivateKeySigner::decrypt_keystore(path, password)
            .map_err(|e| ArbError::CryptoError(e.to_string()))?;

        Ok(Self { signer })
    }

    ///  Save to an encrypted JSON keyfile
    pub fn to_keyfile<P: AsRef<Path>>(&self, path: P, password: &str) -> Result<(), ArbError> {
        let path_ref = path.as_ref();
        let parent = path_ref.parent().unwrap_or_else(|| Path::new("."));

        std::fs::create_dir_all(parent).map_err(|e| ArbError::CryptoError(e.to_string()))?;

        let mut rng = rand_08::thread_rng();

        let result = PrivateKeySigner::encrypt_keystore(
            parent,
            &mut rng,
            self.signer.to_bytes(),
            password,
            None,
        )
        .map_err(|e| ArbError::CryptoError(e.to_string()))?;

        std::fs::write(path_ref, result.1).map_err(|e| ArbError::CryptoError(e.to_string()))?;

        Ok(())
    }

    pub fn address(&self) -> Address {
        Address::from_string(&self.signer.address().to_string()).unwrap()
    }

    /// Sign an arbitrary message (EIP-191).
    pub fn sign_message(&self, message: &[u8]) -> Result<Signature, ArbError> {
        if message.is_empty() {
            return Err(ArbError::EmptyMessage);
        }
        self.signer
            .sign_message_sync(message)
            .map_err(|e| ArbError::CryptoError(e.to_string()))
    }

    /// Sign EIP-712 typed data.
    /// In Rust, we use the SolStruct trait for compile-time safety.
    pub fn sign_typed_data<T: SolStruct>(
        &self,
        data: &T,
        domain: &Eip712Domain,
    ) -> Result<Signature, ArbError> {
        let hash = data.eip712_signing_hash(domain);
        self.signer
            .sign_hash_sync(&hash)
            .map_err(|e| ArbError::CryptoError(e.to_string()))
    }

    /// Sign a transaction request.
    pub fn sign_transaction(&self, tx: &TransactionRequest) -> Result<Signature, ArbError> {
        let hash = tx.signing_hash();
        self.signer
            .sign_hash_sync(&hash)
            .map_err(|e| ArbError::CryptoError(e.to_string()))
    }

    pub fn sign_hash(&self, hash: &B256) -> Result<Signature, ArbError> {
        self.signer
            .sign_hash_sync(hash)
            .map_err(|e| ArbError::CryptoError(e.to_string()))
    }
}

impl fmt::Debug for WalletManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalletManager")
            .field("address", &self.address().checksum())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for WalletManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WalletManager(address={})", self.address().checksum())
    }
}
