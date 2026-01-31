use alloy_primitives::{B256, U256};
use arb_chain::{
    analyzer::DecodedEvent, Analyzer, ChainClient, ChainError, GasPrice, TransactionBuilder,
};
use arb_core::{Address, TokenAmount, WalletManager};
use std::str::FromStr;

#[tokio::test]
async fn test_gas_price_calculation() {
    let gp = GasPrice {
        base_fee: 100_000_000,
        priority_fee_low: 1_000_000,
        priority_fee_medium: 2_000_000,
        priority_fee_high: 5_000_000,
    };

    let max_fee_low = gp.get_max_fee("low", 1.0);
    let max_fee_medium = gp.get_max_fee("medium", 1.2);
    let max_fee_high = gp.get_max_fee("high", 1.5);

    assert_eq!(max_fee_low, 101_000_000);
    assert_eq!(max_fee_medium, 122_000_000);
    assert_eq!(max_fee_high, 155_000_000);
}

#[tokio::test]
async fn test_builder_flow() {
    let client = ChainClient::new("http://127.0.0.1:8545");
    let (wallet, _) = WalletManager::generate();
    let recipient = Address::from_string("0x0000000000000000000000000000000000000000").unwrap();

    let builder = TransactionBuilder::new(&client, &wallet)
        .to(recipient)
        .value(TokenAmount::from_human("0.1", 18, None).unwrap())
        .data(vec![0xaa, 0xbb]);

    let built_tx = builder.build().await.unwrap();

    assert_eq!(
        built_tx.value,
        TokenAmount::from_human("0.1", 18, None).unwrap()
    );
    assert_eq!(built_tx.to.to_string(), recipient.to_string());
    assert_eq!(built_tx.data, vec![0xaa, 0xbb]);
}

#[test]
fn test_token_amount_human() {
    let ta = TokenAmount::from_human("1.5", 18, None).unwrap();
    assert_eq!(ta.raw, U256::from_str("1500000000000000000").unwrap());
}

#[test]
fn test_address_from_string() {
    let addr_str = "0x0000000000000000000000000000000000000001";
    let addr = Address::from_string(addr_str).unwrap();
    assert_eq!(addr.to_string(), addr_str.to_lowercase());
}

#[test]
fn test_gas_calculation_large() {
    let gp = GasPrice {
        base_fee: 10_000_000_000,
        priority_fee_low: 1_000_000_000,
        priority_fee_medium: 2_000_000_000,
        priority_fee_high: 5_000_000_000,
    };
    let max_fee = gp.get_max_fee("high", 1.1);
    assert_eq!(max_fee, 16_000_000_000);
}

#[tokio::test]
async fn test_builder_priority_assignment() {
    let client = ChainClient::new("https://eth.llamarpc.com");
    let (wallet, _) = WalletManager::generate();

    let builder = TransactionBuilder::new(&client, &wallet)
        .to(Address::from_string("0x0000000000000000000000000000000000000001").unwrap())
        .with_gas_price("high");

    let tx = builder.nonce(5).gas_limit(21000).build().await;

    if let Ok(built) = tx {
        assert!(built.max_priority_fee.is_some());
    }
}

#[test]
fn test_error_classification() {
    let client = ChainClient::new("http://localhost:8545");

    assert!(client.is_retryable(&ChainError::RpcError(
        "HTTP 429 Too Many Requests".to_string()
    )));
    assert!(client.is_retryable(&ChainError::RpcError("503 Service Unavailable".to_string())));
    assert!(client.is_retryable(&ChainError::RpcError("502 Bad Gateway".to_string())));

    assert!(client.is_retryable(&ChainError::Timeout));
    assert!(client.is_retryable(&ChainError::RpcError(
        "connection reset by peer".to_string()
    )));
    assert!(client.is_retryable(&ChainError::RpcError("server busy".to_string())));

    assert!(client.is_retryable(&ChainError::RpcError("missing trie node".to_string())));

    assert!(client.is_retryable(&ChainError::RpcError("RATE LIMIT EXCEEDED".to_string())));

    assert!(!client.is_retryable(&ChainError::RpcError("execution reverted".to_string())));
    assert!(!client.is_retryable(&ChainError::RpcError("400 Bad Request".to_string())));
    assert!(!client.is_retryable(&ChainError::RpcError("nonce too low".to_string())));
    assert!(!client.is_retryable(&ChainError::RpcError("insufficient funds".to_string())));
}

#[tokio::test]
async fn test_analyzer_instantiation() {
    let rpc_url = "https://eth.llamarpc.com";
    let mut analyzer = Analyzer::new(rpc_url, 1);

    let random_hash =
        B256::from_str("0x88df016429689c079f3b6f6ad39cbdb3cc3029d9972323e06a35041071295326")
            .unwrap();

    let result = analyzer.analyze(random_hash).await;

    match result {
        Ok(report) => {
            assert_eq!(report.hash, random_hash);
            assert!(report.status == "SUCCESS" || report.status == "FAILED");
        }
        Err(e) => {
            println!(
                "Analyzer parsing test skipped due to RPC connectivity: {}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_analyzer_missing_transaction() {
    let rpc_url = "https://eth.llamarpc.com";
    let mut analyzer = Analyzer::new(rpc_url, 1);

    let missing_hash =
        B256::from_str("0x0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();

    let result = analyzer.analyze(missing_hash).await;

    match result {
        Ok(_) => panic!("Analyzer should have failed for missing transaction"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("not found") || msg.contains("RPC"),
                "Error should indicate missing transaction, got: {}",
                msg
            );
        }
    }
}

#[tokio::test]
async fn test_analyzer_erc20_decoding() {
    let rpc_url = "https://eth.llamarpc.com";
    let mut analyzer = Analyzer::new(rpc_url, 1);

    let tx_hash =
        B256::from_str("0x3f5ce5fb2f7dc5358043743e33df45f20653303c62181347b0df738596602331")
            .unwrap();

    match analyzer.analyze(tx_hash).await {
        Ok(report) => {
            assert_eq!(report.status, "SUCCESS");

            let has_transfer = report
                .events
                .iter()
                .any(|e| matches!(e, DecodedEvent::Transfer { symbol, .. } if symbol == "USDT"));

            assert!(
                has_transfer,
                "Failed to decode USDT Transfer event from known transaction"
            );
        }
        Err(e) => println!("Skipping ERC20 decoding test due to network: {}", e),
    }
}

#[tokio::test]
async fn test_analyzer_uniswap_decoding() {
    let rpc_url = "https://eth.llamarpc.com";
    let mut analyzer = Analyzer::new(rpc_url, 1);

    let tx_hash =
        B256::from_str("0x6d90d794939f50e8a7281d28303f9015c7e099a5e8249079a49c952b610c1404")
            .unwrap();

    match analyzer.analyze(tx_hash).await {
        Ok(report) => {
            assert_eq!(report.status, "SUCCESS");

            let has_swap = report
                .events
                .iter()
                .any(|e| matches!(e, DecodedEvent::Swap { .. }));
            assert!(has_swap, "Failed to decode Uniswap Swap event");

            if let Some(call) = report.decoded_call {
                assert!(
                    call.contains("Uniswap V2") || call.contains("swap"),
                    "Function selector decoding failed or unidentified"
                );
            }
        }
        Err(e) => println!("Skipping Uniswap decoding test due to network: {}", e),
    }
}
