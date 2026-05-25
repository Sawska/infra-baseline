use alloy_primitives::{B256, Bytes};
use alloy_signer::SignerSync;
use alloy_signer_wallet::LocalWallet;
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
    auth_wallet: LocalWallet,
}

impl FlashbotsClient {
    pub fn new(relay_url: &str, auth_private_key: &str) -> Result<Self> {
        let auth_wallet: LocalWallet = auth_private_key.parse()?;
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

    fn sign_payload(&self, payload: &str) -> Result<String> {
        use alloy_primitives::keccak256;
        let hash = keccak256(payload.as_bytes());
        let signature = self.auth_wallet.sign_hash_sync(&hash)?;
        let address = self.auth_wallet.address();
        Ok(format!(
            "{:?}:0x{}",
            address,
            hex::encode(signature.as_bytes())
        ))
    }
}
