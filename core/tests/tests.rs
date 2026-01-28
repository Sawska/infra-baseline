use alloy_primitives::Bytes;
use alloy_sol_types::{sol, Eip712Domain};
use arb_core::{
    Address, ArbError, CanonicalSerializer, TokenAmount, TransactionRequest, WalletManager,
};
use std::collections::BTreeMap;

sol! {
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct Mail {
        address from;
        address to;
        string contents;
    }
}

#[test]
fn test_address_edge_cases() {
    assert!(Address::from_string("invalid").is_err());

    let addr_upper = Address::from_string("0x742D35CC6634C0532925A3B844BC454E4438F44E").unwrap();
    let addr_lower = Address::from_string("0x742d35cc6634c0532925a3b844bc454e4438f44e").unwrap();
    assert_eq!(addr_upper, addr_lower);
}

#[test]
fn test_token_amount_arithmetic() {
    let a = TokenAmount::from_human("1.5", 18, None).unwrap();
    assert_eq!(a.raw.to_string(), "1500000000000000000");

    let b = TokenAmount::from_human("2.5", 18, None).unwrap();
    let c = (a + b).unwrap();
    assert_eq!(c.to_human().to_string(), "4");

    let d = TokenAmount::from_human("1.0", 6, None).unwrap();
    let e = TokenAmount::from_human("1.0", 18, None).unwrap();
    assert!((d + e).is_err());
}

#[test]
fn test_wallet_security() {
    let (wallet, pkey) = WalletManager::generate();

    let debug_str = format!("{:?}", wallet);
    let display_str = format!("{}", wallet);
    assert!(!debug_str.contains(&pkey));
    assert!(!display_str.contains(&pkey));

    let empty: [u8; 0] = [];
    assert!(wallet.sign_message(&empty).is_err());
}

#[test]
fn test_eip712_signing() {
    let (wallet, _) = WalletManager::generate();

    let domain = Eip712Domain::default();
    let mail = Mail {
        from: wallet.address().0,
        to: alloy_primitives::address!("0000000000000000000000000000000000000000"),
        contents: "Hello, Alloy!".to_string(),
    };

    let signature = wallet.sign_typed_data(&mail, &domain);
    assert!(signature.is_ok());
}

#[test]
fn test_transaction_hashing() {
    let addr = Address::from_string("0x742d35cc6634c0532925a3b844bc454e4438f44e").unwrap();
    let tx = TransactionRequest {
        to: addr,
        value: TokenAmount::from_human("1.0", 18, None).unwrap(),
        data: Bytes::from(vec![1, 2, 3]),
        nonce: Some(1),
        gas_limit: Some(21000),
        max_fee_per_gas: Some(100_000_000_000),
        max_priority_fee: Some(1_000_000_000),
        chain_id: 1,
    };

    let hash = tx.signing_hash();
    assert_ne!(hash, alloy_primitives::B256::ZERO);
}

#[test]
fn test_canonical_serialization() {
    let mut map1 = BTreeMap::new();
    map1.insert("b", 2);
    map1.insert("a", 1);

    let bytes1 = CanonicalSerializer::serialize(&map1).unwrap();
    assert_eq!(std::str::from_utf8(&bytes1).unwrap(), r#"{"a":1,"b":2}"#);

    let emoji = "🚀";
    let bytes2 = CanonicalSerializer::serialize(&emoji).unwrap();
    assert!(std::str::from_utf8(&bytes2).is_ok());

    assert!(CanonicalSerializer::verify_determinism(&map1, 100));
}

#[test]
fn test_mismatched_decimals_error() {
    let usdc = TokenAmount::from_human("100", 6, Some("USDC".into())).unwrap();
    let eth = TokenAmount::from_human("1", 18, Some("ETH".into())).unwrap();

    let result = usdc + eth;
    match result {
        Err(ArbError::MismatchedDecimals(6, 18)) => (),
        _ => panic!("Should have failed with MismatchedDecimals error"),
    }
}

#[test]
fn test_serializer_consistency() {
    #[derive(serde::Serialize)]
    struct Nested {
        a: i32,
        b: Vec<String>,
    }

    let obj1 = Nested {
        a: 1,
        b: vec!["z".into(), "y".into()],
    };
    let hash1 = CanonicalSerializer::hash(&obj1).unwrap();

    let obj2 = Nested {
        a: 1,
        b: vec!["z".into(), "y".into()],
    };
    let hash2 = CanonicalSerializer::hash(&obj2).unwrap();

    assert_eq!(
        hash1, hash2,
        "Hashes must be identical for identical structures"
    );
}

#[test]
fn test_error_display() {
    let err = ArbError::EnvVarMissing("PRIVATE_KEY".to_string());
    assert_eq!(err.to_string(), "Missing environment variable: PRIVATE_KEY");
}

#[test]
fn test_serializer_determinism_stress_1000() {
    let mut map = BTreeMap::new();
    for i in 0..50 {
        map.insert(format!("key_{}", i), i);
    }

    assert!(
        CanonicalSerializer::verify_determinism(&map, 1000),
        "CanonicalSerializer produced different outputs across 1000 iterations"
    );
}
