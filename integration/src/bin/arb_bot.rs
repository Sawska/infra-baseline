use alloy_primitives::{Address as AlloyAddress, U256};
use alloy_sol_types::{SolCall, sol};
use anyhow::Result;
use arb_chain::client::ChainClient;
use arb_core::WalletManager;
use arb_core::types::{Address, TokenAmount, TransactionRequest};
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use executor::engine::{Executor, ExecutorConfig, ExecutorState};
use executor::metrics::gather_metrics;
use inventory::tracker::{InventoryTracker, Venue};
use log::{error, info, warn};
use pricing::amm::Pool;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use strategy::fees::FeeStructure;
use strategy::generator::{GeneratorConfig, PairMetadata, SignalGenerator};
use strategy::scorer::{ScorerConfig, SignalScorer};
use tokio::sync::Mutex;
use tokio::time::sleep;
use warp::Filter;

sol! {
    function balanceOf(address account) external view returns (uint256);
}

struct ArbBot {
    shared_inventory: Arc<Mutex<InventoryTracker>>,
    exchange: Arc<ExchangeClient>,
    generator: SignalGenerator,
    scorer: SignalScorer,
    executor: Executor,
    pairs: Vec<String>,
    trade_size: f64,
    running: bool,
    chain_client: Arc<ChainClient>,
    wallet: Arc<WalletManager>,
    token_addresses: HashMap<String, Address>,
}

impl ArbBot {
    pub async fn new(config: BotConfig) -> Result<Self> {
        info!("Initializing ArbBot...");

        let exchange = Arc::new(
            ExchangeClient::new(
                ExchangeConfig::from_env(ExchangeType::Binance)?,
                ExchangeType::Binance,
            )
            .await?,
        );

        let rpc_url = env::var("RPC_URL_LOCAL")?;
        let chain_client = Arc::new(ChainClient::new(&rpc_url));
        let wallet = Arc::new(WalletManager::from_env("PRIVATE_KEY_TEST")?);
        let shared_inventory = Arc::new(Mutex::new(InventoryTracker::new(Some(&wallet))));

        let mut generator = SignalGenerator::new(
            exchange.clone(),
            shared_inventory.clone(),
            FeeStructure::default(),
            GeneratorConfig {
                min_spread_bps: 10.0,
                min_profit_usd: 0.1,
                ..Default::default()
            },
        );

        let mut pools_map: HashMap<String, Pool> = HashMap::new();

        for pair in &config.pairs {
            let pool_addr = config
                .dex_pools
                .get(pair)
                .ok_or_else(|| anyhow::anyhow!("Missing pool address for {}", pair))?;

            let address = Address::from_string(pool_addr)?;
            let pool = Pool::from_chain(address, &chain_client, None).await?;
            let (base, quote) = pool.tokens();

            generator.register_pair(
                pair.clone(),
                PairMetadata {
                    pool: pool.clone(),
                    base_token: base,
                    quote_token: quote,
                },
            );

            pools_map.insert(pair.clone(), pool);
        }

        let scorer = SignalScorer::new(Some(ScorerConfig::default()));

        let executor = Executor::new(
            exchange.clone(),
            Some(chain_client.clone()),
            Some(wallet.clone()),
            config.token_addresses.clone(),
            pools_map,
            shared_inventory.clone(),
            Some(ExecutorConfig {
                simulation_mode: config.simulation,
                use_flashbots: !config.simulation,
                dex_router_address: config.dex_router,
                ..Default::default()
            }),
            config.webhook_url,
        );

        let bot = Self {
            shared_inventory,
            exchange,
            generator,
            scorer,
            executor,
            pairs: config.pairs,
            trade_size: config.trade_size,
            running: false,
            chain_client,
            wallet,
            token_addresses: config.token_addresses,
        };

        bot.sync_binance_balances().await?;
        bot.sync_wallet_balances().await?;

        Ok(bot)
    }

    /// Fetches balances from the blockchain for all configured tokens
    pub async fn sync_wallet_balances(&self) -> Result<()> {
        let address = self.wallet.address();
        let mut wallet_amounts = Vec::new();

        let eth_balance = self.chain_client.get_balance(address).await?;
        wallet_amounts.push(TokenAmount {
            raw: eth_balance.raw,
            decimals: 18,
            symbol: Some("ETH".to_string()),
        });

        for (symbol, token_addr) in &self.token_addresses {
            if symbol == "ETH" {
                continue;
            }

            let call = balanceOfCall {
                account: AlloyAddress::from(*address.0),
            };
            let data = call.abi_encode();

            let req = TransactionRequest {
                to: *token_addr,
                from: Some(address),
                data: data.into(),
                nonce: None,
                gas_limit: None,
                max_fee_per_gas: None,
                max_priority_fee: None,
                chain_id: 1,
                value: TokenAmount {
                    raw: U256::ZERO,
                    decimals: 18,
                    symbol: None,
                },
            };

            match self.chain_client.call(&req, None).await {
                Ok(res) => {
                    if let Ok(balance) = balanceOfCall::abi_decode_returns(&res) {
                        let decimals = if symbol.contains("USDT") || symbol.contains("USDC") {
                            6
                        } else if symbol.contains("BTC") {
                            8
                        } else {
                            18
                        };

                        wallet_amounts.push(TokenAmount {
                            raw: balance,
                            decimals,
                            symbol: Some(symbol.clone()),
                        });
                    }
                }
                Err(e) => warn!("Failed to fetch balance for {}: {:?}", symbol, e),
            }
        }

        let mut inventory = self.shared_inventory.lock().await;
        inventory.update_from_wallet(wallet_amounts);
        Ok(())
    }

    pub async fn sync_binance_balances(&self) -> Result<()> {
        let raw_balances = self.exchange.fetch_balance().await?;
        let mut mapped_balances = HashMap::new();

        for (asset, balance) in raw_balances {
            let free_decimal = balance.free.to_human();
            let locked_decimal = balance.locked.to_human();

            if free_decimal.is_zero() && locked_decimal.is_zero() {
                continue;
            }

            let internal_asset = if asset == "ETH" {
                "WETH".to_string()
            } else {
                asset.clone()
            };

            mapped_balances.insert(internal_asset, (free_decimal, locked_decimal));
        }

        let mut inventory = self.shared_inventory.lock().await;
        inventory.update_from_cex(Venue::Cex, mapped_balances);
        Ok(())
    }

    /// Internal CEX Rebalancing: Swaps excess USDC for ETH/USDT if low
    pub async fn check_and_rebalance(&self) -> Result<()> {
        let (usdc_bal, usdt_bal, eth_bal) = {
            let inv = self.shared_inventory.lock().await;
            (
                inv.get_available(Venue::Cex, "USDC")
                    .to_f64()
                    .unwrap_or(0.0),
                inv.get_available(Venue::Cex, "USDT")
                    .to_f64()
                    .unwrap_or(0.0),
                inv.get_available(Venue::Cex, "WETH")
                    .to_f64()
                    .unwrap_or(0.0),
            )
        };

        if usdc_bal > 5000.0 {
            if eth_bal < 5.0 {
                info!(
                    "📉 REBALANCE: Low ETH ({:.2} < 1.0). Buying 3.0 ETH with USDC...",
                    eth_bal
                );
                match self.exchange.fetch_order_book("ETH/USDC", 5).await {
                    Ok(book) => {
                        let price = book.best_ask.0 * Decimal::from_f64(1.05).unwrap();
                        let amount =
                            TokenAmount::from_human("3.0", 18, Some("ETH".to_string())).unwrap();

                        match self
                            .exchange
                            .create_limit_ioc_order("ETH/USDC", "buy", amount, price)
                            .await
                        {
                            Ok(_) => {
                                info!("✅ REBALANCE: Bought 3.0 ETH");
                                self.sync_binance_balances().await?;
                            }
                            Err(e) => error!("Failed to buy ETH: {:?}", e),
                        }
                    }
                    Err(e) => error!("Failed to fetch orderbook for ETH buy: {:?}", e),
                }
            }

            if usdt_bal < 1500.0 {
                info!(
                    "📉 REBALANCE: Low USDT ({:.2} < 1500). Selling 1.5 ETH for USDT...",
                    usdt_bal
                );
                match self.exchange.fetch_order_book("ETH/USDT", 5).await {
                    Ok(book) => {
                        let price = book.best_bid.0 * Decimal::from_f64(0.95).unwrap();
                        let amount =
                            TokenAmount::from_human("1.5", 18, Some("ETH".to_string())).unwrap();

                        match self
                            .exchange
                            .create_limit_ioc_order("ETH/USDT", "sell", amount, price)
                            .await
                        {
                            Ok(_) => {
                                info!("✅ REBALANCE: Sold 1.5 ETH for USDT");
                                self.sync_binance_balances().await?;
                            }
                            Err(e) => error!("Failed to sell ETH for USDT: {:?}", e),
                        }
                    }
                    Err(e) => error!("Failed to fetch orderbook for ETH sell: {:?}", e),
                }
            }
        }
        Ok(())
    }

    pub async fn run(&mut self) {
        self.running = true;
        info!("Bot starting...");

        while self.running {
            if let Err(e) = self.sync_binance_balances().await {
                error!("Binance Sync Failed: {:?}", e);
            }
            if let Err(e) = self.sync_wallet_balances().await {
                error!("Wallet Sync Failed: {:?}", e);
            }

            if let Err(e) = self.tick().await {
                error!("Tick error: {}", e);
                sleep(Duration::from_secs(5)).await;
            } else {
                sleep(Duration::from_secs(1)).await;
            }
        }
    }

    async fn tick(&mut self) -> Result<()> {
        if self.executor.is_circuit_open().await {
            info!("Circuit breaker open");
            return Ok(());
        }

        if let Err(e) = self.check_and_rebalance().await {
            warn!("Rebalance check failed: {:?}", e);
        }

        self.generator.clear_queue();

        for pair in &self.pairs {
            self.generator.queue_candidate(pair, self.trade_size).await;
        }

        if let Some(best_signal) = self.generator.pop_top_signal() {
            info!(
                "Executing best signal: {} spread={:.1}bps",
                best_signal.pair, best_signal.spread_bps
            );

            let ctx = self.executor.execute(best_signal.clone()).await;
            let success = ctx.state == ExecutorState::Done;

            self.scorer.record_result(best_signal.pair.clone(), success);

            if success {
                info!("SUCCESS: PnL=${:.2}", ctx.actual_net_pnl.unwrap_or(0.0));
            } else {
                warn!(
                    "FAILED: {:?}",
                    ctx.error.as_deref().unwrap_or("Unknown error")
                );
            }
        }

        Ok(())
    }
}

struct BotConfig {
    pairs: Vec<String>,
    trade_size: f64,
    simulation: bool,
    webhook_url: Option<String>,
    dex_router: Option<String>,
    dex_pools: HashMap<String, String>,
    token_addresses: HashMap<String, Address>,
}

#[tokio::main]
async fn main() {
    env_logger::init();
    tokio::spawn(async move {
        let metrics_route = warp::path("metrics").map(gather_metrics).boxed();

        info!("Metrics server starting on http://localhost:3030/metrics");

        let addr: std::net::SocketAddr = ([127, 0, 0, 1], 3030).into();

        warp::serve(metrics_route).run(addr).await;
    });

    let mut dex_pools = HashMap::new();
    dex_pools.insert(
        "WETH/USDT".to_string(),
        "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852".to_string(),
    );
    dex_pools.insert(
        "WETH/USDC".to_string(),
        "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".to_string(),
    );

    let mut token_addresses = HashMap::new();
    token_addresses.insert(
        "WETH".to_string(),
        Address::from_string("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap(),
    );
    token_addresses.insert(
        "USDT".to_string(),
        Address::from_string("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap(),
    );
    token_addresses.insert(
        "USDC".to_string(),
        Address::from_string("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48").unwrap(),
    );

    let config = BotConfig {
        pairs: vec!["WETH/USDT".to_string(), "WETH/USDC".to_string()],
        trade_size: 0.1,
        simulation: true,
        webhook_url: Some("http://127.0.0.1:3031/webhook".to_string()),
        dex_router: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
        dex_pools,
        token_addresses,
    };

    match ArbBot::new(config).await {
        Ok(mut bot) => bot.run().await,
        Err(e) => error!("Bot initialization failed: {}", e),
    }
}
