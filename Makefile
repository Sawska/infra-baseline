.PHONY: run-analyzer run-sepolia test lint clean build

run-analyzer:
	cargo run --bin analyzer 0x6a45c9efcf942a48ba0e26441cf9db7f2ae6cc5f9731d3c7b7fc31692ab3cec0 --json

run-sepolia:
	cargo run -p arb-chain --bin integration_test

test:
	cargo test

lint:
	cargo fmt -- --check
	cargo clippy -- -D warnings

build:
	cargo build

clean:
	cargo clean

password:
	cargo run --bin password

docs:
	cargo doc --no-deps --all-features

open-docs:
	cargo doc --no-deps --all-features --open
