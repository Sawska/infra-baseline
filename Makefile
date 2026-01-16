run:
	cargo run

test:
	cargo test

lint:
	cargo fmt -- --check
	cargo clippy -- -D warnings
