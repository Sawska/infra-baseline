# Arb Execution Engine

A high-performance, modular execution engine for Ethereum arbitrage trading, written in Rust.

This engine is designed with a focus on determinism, type safety, and reliability. It separates business logic from network interaction to ensure that signing and serialization remain secure and predictable, even in the event of network failures.

## Key Features

### Core (`arb-core`)

- **Secure Wallet Management**
  Loads keys from environment or encrypted keystores. Private keys are redacted from all debug logs and string representations.

- **Deterministic Serialization**
  Custom `CanonicalSerializer` that strictly sorts JSON keys and rejects floating point numbers to ensure consensus compatibility.

- **Strict Typing**
  Custom `TokenAmount` and `Address` types to prevent precision loss and handle checksums automatically.

### Chain (`arb-chain`)

- **Resilient RPC Client**
  Automatic failover and retry logic. If the primary RPC node fails, the client seamlessly switches to backup providers.

- **Fluent Transaction Builder**
  A type-safe builder pattern for estimating gas, managing nonces, and signing transactions
  (`.to().value().send_and_wait()`).

- **Transaction Analyzer CLI**
  A standalone tool to dissect Mainnet transactions, decoding DeFi function calls (Uniswap, ERC20) and summarizing token swaps.

## Architecture

The project is organized as a Cargo workspace:

```

arb-execution-engine/
├── core/       # Pure logic (Wallet, Types, Serialization). No network dependencies.
├── chain/      # Network logic (RPC Client, Tx Builder, Analyzer). Depends on `core`.
└── .env        # Configuration (Private Keys, RPC URLs)

````

## Getting Started

### Prerequisites

- Rust & Cargo (v1.70+)
- An Ethereum Node RPC URL (Alchemy, Infura, or public node)
- A Sepolia private key (for integration tests)

### Installation

Clone the repository:

```bash
git clone https://github.com/yourusername/arb-execution-engine.git
cd arb-execution-engine
````

Set up configuration:

```bash
cp .env_example .env
```

Edit `.env` to include:

* `PRIVATE_KEY` (no `0x` prefix)
* `SEPOLIA_RPC`

Build the project:

```bash
make build
```

## Usage

The project includes a `Makefile` for common tasks.

### 1. Run the Transaction Analyzer

Analyze any Ethereum Mainnet transaction hash to see gas usage, function decoding, and token transfers.

```bash
# Example: Analyze a Uniswap V2 Swap
make run-analyzer
```

Or run manually:

```bash
cargo run -p arb-chain --bin analyzer -- <TX_HASH>
```

### 2. Run Live Integration Test (Sepolia)

This script performs a full lifecycle test on the Sepolia testnet:

* Connects to RPC
* Checks wallet balance
* Builds and estimates a transaction
* Signs and verifies the signature locally
* Broadcasts to the network and waits for confirmation

```bash
make run-sepolia
```

### 3. Run Test Suite

Executes unit tests and local integration tests.

```bash
make test
```

## Testing Strategy

We maintain a strict testing regime to ensure financial safety:

* **Unit Tests (>15)**
  Covers edge cases in `core`, including Unicode handling, large integer serialization, and gas math.

* **Security Tests**
  Automated checks ensure that `println!("{:?}", wallet)` never leaks secrets.

* **Local Integration**
  `chain/tests/local_flow.rs` verifies the Builder → Signer → RLP encoding pipeline without hitting the network.

* **Live Integration**
  `integration_test.rs` verifies end-to-end functionality on a testnet.

## 🛡️ Design Decisions

* **No Floating Point Math**
  Arbitrage relies on exact precision. `f64` is rejected in the `CanonicalSerializer`. All math uses `U256` or normalized decimal types.

* **Failover Client**
  Speed is critical. The `ChainClient` maintains multiple providers and immediately rotates on errors or timeouts.

* **Workspace Structure**
  Separating `core` (logic) from `chain` (IO) prevents circular dependencies and allows extremely fast logic tests in CI.

```
