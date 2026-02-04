.PHONY: fmt run-analyzer run-impact run-sepolia test lint clean build exchange-analyzer rebalancer-check pln-reporter rebalancer-plan realtime-dashboard

SYMBOL ?= "ETH/USDT"
DEPTH ?= 20

run-analyzer:
	cargo run --bin analyzer 0x6a45c9efcf942a48ba0e26441cf9db7f2ae6cc5f9731d3c7b7fc31692ab3cec0 --json
exchange-analyzer:
	cargo run -p exchange -- $(SYMBOL) --depth $(DEPTH)
rebalancer-check:
	cargo run --bin rebalancer_cli -- check
rebalancer-plan:
	cargo run --bin rebalancer_cli -- plan ETH
pln-reporter:
	cargo run --bin pnl_report -- summary --chart
realtime-dashboard:
	cargo run --bin inventory_dashboard
arb_checker:
	cargo run --bin arb_checker

run-impact-1:
	cargo run -p pricing --bin impact_analyzer -- \
  0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc \
  --token-in USDC \
  --sizes 1000,10000,100000 \
  --rpc https://eth.merkle.io

run-impact-2:
	cargo run -p pricing --bin impact_analyzer -- \
  0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852 \
  --token-in USDT \
  --sizes 1000,10000,100000 \
  --rpc https://eth.merkle.io

run-impact-3:
	cargo run -p pricing --bin impact_analyzer -- \
  0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc \
  --token-in USDC \
  --start-block 24343905 \
  --end-block 24343905 \
  --sizes 2500 \
  --rpc https://eth.merkle.io

run-router:
	cargo run -p pricing --bin router_demo -- \
	--pools 0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc,0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11,0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852 \
	--token-in USDC --token-out USDT --amount 1000

run-monitor:
	cargo run -p pricing --bin monitor_demo

run-sepolia:
	cargo run -p arb-chain --bin integration_test

test:
	cargo test

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace -- -D warnings

build:
	cargo build

clean:
	cargo clean

password:
	cargo run --bin password

docs:
	cargo doc --no-deps --all-features

fmt:
	cargo fmt --all

open-docs:
	cargo doc --no-deps --all-features --open
