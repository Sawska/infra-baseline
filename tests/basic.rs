use std::env;

#[test]
fn sanity_check() {
    assert_eq!(2 + 2, 4);
}

#[test]
fn missing_env_fails() {
    env::remove_var("RPC_URL");
    let result = std::env::var("RPC_URL");
    assert!(result.is_err());
}
