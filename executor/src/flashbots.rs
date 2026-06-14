use alloy_primitives::hex;
use alloy_primitives::{B256, Bytes};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Result, anyhow};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FlashbotsBundleRequest {
    txs: Vec<String>,
    block_number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reverting_tx_hashes: Vec<String>,
}

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: Vec<FlashbotsBundleRequest>,
}

#[derive(Deserialize, Debug)]
pub struct FlashbotsResponse {
    pub result: Option<FlashbotsResult>,
    pub error: Option<FlashbotsError>,
}

#[derive(Deserialize, Debug)]
pub struct FlashbotsResult {
    #[serde(rename = "bundleHash")]
    pub bundle_hash: B256,
}

#[derive(Deserialize, Debug)]
pub struct FlashbotsError {
    pub message: String,
    pub code: i32,
}

pub struct FlashbotsClient {
    client: Client,
    relay_url: String,
    auth_wallet: PrivateKeySigner,
}

impl FlashbotsClient {
    pub fn new(relay_url: &str, auth_private_key: &str) -> Result<Self> {
        let auth_wallet: PrivateKeySigner = auth_private_key.parse()?;
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        Ok(Self {
            client,
            relay_url: relay_url.to_string(),
            auth_wallet,
        })
    }

    pub async fn send_bundle(&self, signed_txs: Vec<Bytes>, target_block: u64) -> Result<B256> {
        let hex_txs: Vec<String> = signed_txs
            .into_iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let bundle_req = FlashbotsBundleRequest {
            txs: hex_txs,
            block_number: format!("0x{:x}", target_block),
            min_timestamp: None,
            max_timestamp: None,
            reverting_tx_hashes: vec![],
        };

        let rpc_req = RpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "eth_sendBundle",
            params: vec![bundle_req],
        };

        let body = serde_json::to_string(&rpc_req)?;
        let signature = self.sign_payload(&body)?;

        let response = self
            .client
            .post(&self.relay_url)
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-Flashbots-Signature", signature)
            .body(body)
            .send()
            .await?;

        let fb_res: FlashbotsResponse = response.json().await?;

        if let Some(err) = fb_res.error {
            return Err(anyhow!("Flashbots error {}: {}", err.code, err.message));
        }

        fb_res
            .result
            .map(|r| r.bundle_hash)
            .ok_or_else(|| anyhow!("Invalid response from Flashbots relay"))
    }

    /// Simulate a bundle against the relay via `eth_callBundle` before submitting it.
    ///
    /// Returns the raw relay result. Callers should treat a non-empty `error`, or any
    /// per-transaction `error`/`revert` entry in `results`, as a failed integrity check
    /// and skip submission.
    pub async fn simulate_bundle(
        &self,
        signed_txs: Vec<Bytes>,
        target_block: u64,
    ) -> Result<serde_json::Value> {
        let hex_txs: Vec<String> = signed_txs
            .into_iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let params = serde_json::json!([{
            "txs": hex_txs,
            "blockNumber": format!("0x{:x}", target_block),
            "stateBlockNumber": "latest",
        }]);
        let body = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_callBundle",
            "params": params,
        }))?;
        let signature = self.sign_payload(&body)?;

        let response = self
            .client
            .post(&self.relay_url)
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-Flashbots-Signature", signature)
            .body(body)
            .send()
            .await?;

        let value: serde_json::Value = response.json().await?;
        if let Some(err) = value.get("error") {
            return Err(anyhow!("Flashbots simulation error: {}", err));
        }
        Ok(value)
    }

    /// Build the `X-Flashbots-Signature` header value for a request body.
    ///
    /// Flashbots authenticates the body with an EIP-191 `personal_sign` over the
    /// **0x-prefixed hex string** of `keccak256(body)` — not the raw 32-byte digest.
    /// Signing the digest directly (e.g. via `sign_hash`) produces a signature the relay
    /// rejects as `unauthorized`/invalid signer, so the EIP-191 message form is required.
    fn sign_payload(&self, payload: &str) -> Result<String> {
        use alloy_primitives::keccak256;
        let hash = keccak256(payload.as_bytes());
        let message = format!("0x{}", hex::encode(hash));
        let signature = self.auth_wallet.sign_message_sync(message.as_bytes())?;
        let address = self.auth_wallet.address();
        Ok(format!(
            "{:?}:0x{}",
            address,
            hex::encode(signature.as_bytes())
        ))
    }
}
