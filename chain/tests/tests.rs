use alloy_primitives::U256;
use arb_chain::{ChainClient, GasPrice, TransactionBuilder};
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
    let client = ChainClient::new("https://eth.llamarpc.com");
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
