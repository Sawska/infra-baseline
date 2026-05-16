use alloy_primitives::{Address as AlloyAddress, U256};
use alloy_sol_types::{SolCall, sol};
use anyhow::Result;
use arb_chain::client::ChainClient;
use arb_core::WalletManager;
use arb_core::types::{Address, TokenAmount, TransactionRequest};
use exchange::client::{ExchangeClient, ExchangeType};
use exchange::config::ExchangeConfig;
use executor::engine::{Executor, ExecutorConfig, ExecutorState};
use executor::gas_oracle::{GasOracle, gas_units as gas_units_table, safety_check_with_gas};
use executor::kill_switch::AutoKillSwitch;
use executor::metrics::gather_metrics;
use executor::monitoring;
use executor::position_limits::{RiskLimits, RiskManager};
use executor::safety::safety_check;
use executor::telegram_alert::TelegramAlert;
use executor::validator;
use inventory::tracker::{InventoryTracker, Venue};
use log::{error, info, warn};
use pricing::amm::Pool;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use strategy::fees::FeeStructure;
use strategy::generator::{GeneratorConfig, PairMetadata, SignalGenerator};
use strategy::scorer::{ScorerConfig, SignalScorer};
use tokio::sync::Mutex;
use tokio::time::sleep;
use warp::Filter;

const KILL_SWITCH_FILE: &str = "/tmp/arb_bot_kill";
const HEARTBEAT_FILE: &str = "/tmp/arb_bot_heartbeat";
const PNL_HISTORY_FILE: &str = "pnl_history.csv";
const PNL_CHART_FILE: &str = "pnl_chart.html";

sol! {
    function balanceOf(address account) external view returns (uint256);
}

pub fn is_kill_switch_active() -> bool {
    Path::new(KILL_SWITCH_FILE).exists()
}

#[derive(Debug, Clone)]
pub struct PerPairConfig {
    pub trade_size: f64,
    pub min_spread_bps: f64,
    pub min_profit_usd: f64,
    pub fee_structure: FeeStructure,
    pub gas_units: u64,
}

impl PerPairConfig {
    pub fn pepe_default() -> Self {
        Self {
            trade_size: 1_250_000.0,
            min_spread_bps: 130.0,
            min_profit_usd: 0.01,
            fee_structure: FeeStructure {
                cex_taker_bps: 10.0,
                dex_swap_bps: 1.0,
                gas_cost_usd: 0.07,
            },
            gas_units: gas_units_table::TWO_HOP_V3,
        }
    }

    pub fn eth_default() -> Self {
        Self {
            trade_size: 0.0026,
            min_spread_bps: 20.0,
            min_profit_usd: 0.02,
            fee_structure: FeeStructure {
                cex_taker_bps: 10.0,
                dex_swap_bps: 5.0,
                gas_cost_usd: 0.02,
            },
            gas_units: gas_units_table::UNISWAP_V3_SWAP,
        }
    }
}

struct ArbBot {
    shared_inventory: Arc<Mutex<InventoryTracker>>,
    exchange: Arc<ExchangeClient>,
    generator: SignalGenerator,
    scorer: SignalScorer,
    executor: Executor,
    risk_manager: RiskManager,
    validator: validator::PreTradeValidator,
    telegram: Arc<TelegramAlert>,
    kill_switch: AutoKillSwitch,
    pairs: Vec<String>,
    running: bool,
    trading_active: bool,
    dry_run: bool,
    chain_client: Arc<ChainClient>,
    wallet: Arc<WalletManager>,
    token_addresses: HashMap<String, Address>,
    start_time: Instant,
    last_trade_time: Option<Instant>,
    session_pnl: f64,
    cumulative_pnl: f64,
    error_timestamps: VecDeque<Instant>,
    daily_trades: Vec<f64>,
    pair_configs: HashMap<String, PerPairConfig>,
    gas_oracle: GasOracle,
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

        let rpc_url = env::var("ARBITRUM_RPC_URL")?;
        let chain_client = Arc::new(ChainClient::new(&rpc_url));
        let wallet = Arc::new(WalletManager::from_env("PRIVATE_KEY")?);
        let shared_inventory = Arc::new(Mutex::new(InventoryTracker::new(Some(&wallet))));

        let mut generator = SignalGenerator::new(
            exchange.clone(),
            shared_inventory.clone(),
            FeeStructure::default(),
            GeneratorConfig {
                min_spread_bps: 50.0,
                min_profit_usd: 0.01,
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
                dex_router_address: config.dex_router.clone(),
                dex_router_v3: config.dex_router,
                ..Default::default()
            }),
            config.webhook_url,
        );

        let risk_limits = RiskLimits {
            max_trade_usd: 10.0,
            max_trade_pct: 0.20,
            max_position_per_token: 10.0,
            max_open_positions: 1,
            max_drawdown_pct: 0.20,
            max_daily_loss: 10.0,
            max_trades_per_hour: 20,
            consecutive_loss_limit: 3,
            max_loss_per_trade: 5.0,
        };

        let risk_manager = RiskManager::new(risk_limits, 71.00);
        let validator = validator::PreTradeValidator::default();
        let kill_switch = AutoKillSwitch::new();
        let telegram = Arc::new(TelegramAlert::new(
            env::var("TELEGRAM_CHAT_ID").unwrap_or_default(),
        ));

        let path = Path::new(PNL_HISTORY_FILE);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file_exists = path.exists();

        let mut file = OpenOptions::new().create(true).append(true).open(path)?;

        if !file_exists {
            writeln!(file, "Timestamp,SessionPnL,CumulativePnL,Drawdown")?;
        }

        let bot = Self {
            shared_inventory,
            exchange,
            generator,
            scorer,
            executor,
            risk_manager,
            validator,
            telegram,
            kill_switch,
            pairs: config.pairs,
            running: false,
            trading_active: false,
            dry_run: config.dry_run,
            chain_client,
            wallet,
            token_addresses: config.token_addresses,
            start_time: Instant::now(),
            last_trade_time: None,
            session_pnl: 0.0,
            cumulative_pnl: 0.0,
            error_timestamps: VecDeque::new(),
            daily_trades: Vec::new(),
            pair_configs: config.pair_configs,
            gas_oracle: GasOracle::new(0.1, 0.1, 3_000.0),
        };

        bot.sync_binance_balances().await?;
        bot.sync_wallet_balances().await?;

        let state_payload = serde_json::json!({
            "trading_active": bot.trading_active,
            "dry_run": bot.dry_run,
            "session_pnl": bot.session_pnl,
            "cumulative_pnl": bot.cumulative_pnl,
            "capital": bot.risk_manager.current_capital,
            "drawdown": 0.0,
            "trades_today": 0,
            "wins": 0,
            "losses": 0,
            "uptime_secs": 0
        });
        bot.telegram.update_bot_state(state_payload).await;

        Ok(bot)
    }

    fn update_heartbeat(&self) {
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let timestamp = since_the_epoch.as_secs();

        if let Err(e) = fs::write(HEARTBEAT_FILE, timestamp.to_string()) {
            error!("Failed to update heartbeat: {}", e);
        }
    }

    async fn emergency_flatten(&self) {
        warn!("EMERGENCY FLATTEN TRIGGERED: Selling all CEX positions!");
        self.telegram
            .send("🚨 <b>EMERGENCY FLATTEN TRIGGERED</b>", true)
            .await;

        if self.dry_run {
            warn!("Dry run enabled. Skipping actual sell orders.");
            return;
        }

        let balances = match self.exchange.fetch_balance().await {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to fetch balances for flatten: {}", e);
                return;
            }
        };

        for pair in &self.pairs {
            let parts: Vec<&str> = pair.split('/').collect();
            if parts.len() != 2 {
                continue;
            }
            let base_asset = parts[0];

            if let Some(balance) = balances.get(base_asset) {
                let raw_amount = balance.free.to_human().to_f64().unwrap_or(0.0);
                let floor_amount = raw_amount.floor();

                if floor_amount > 0.0 {
                    info!(
                        "Flattening {} (Original: {}, Floor: {})",
                        base_asset, raw_amount, floor_amount
                    );

                    let amount = match TokenAmount::from_human(&floor_amount.to_string(), 18, None)
                    {
                        Ok(a) => a,
                        Err(_) => continue,
                    };

                    info!("Placing emergency MARKET sell for {}", pair);
                    match self
                        .exchange
                        .create_market_order(pair, "sell", amount)
                        .await
                    {
                        Ok(order) => info!("Flattened {}: Order ID {}", base_asset, order.id),
                        Err(e) => {
                            error!("Failed to flatten {} via market order: {:?}", base_asset, e)
                        }
                    }
                } else if raw_amount > 0.0 {
                    info!(
                        "Dust balance detected for {} ({}), skipping flatten.",
                        base_asset, raw_amount
                    );
                }
            }
        }
    }

    fn generate_pnl_chart_html(&self) {
        let mut file = match fs::File::open(PNL_HISTORY_FILE) {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_err() {
            return;
        }

        let mut dates = Vec::new();
        let mut pnls = Vec::new();
        let mut drawdowns = Vec::new();

        for line in contents.lines().skip(1) {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 4 {
                if let Ok(ts) = parts[0].parse::<u64>() {
                    dates.push(ts * 1000);
                }
                if let Ok(cp) = parts[2].parse::<f64>() {
                    pnls.push(cp);
                }
                if let Ok(dd) = parts[3].parse::<f64>() {
                    drawdowns.push(dd);
                }
            }
        }

        let html_content = format!(
            r#"
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Arb Bot PnL Dashboard</title>
    <script src="https://cdn.jsdelivr.net/npm/echarts@5.4.3/dist/echarts.min.js"></script>
    <style>
        body {{ background-color: #1a1a1a; color: #e0e0e0; font-family: sans-serif; margin: 20px; }}
        #chart-container {{ width: 100%; height: 600px; }}
    </style>
</head>
<body>
    <h2>🚀 Arbitrage Bot Performance</h2>
    <div id="chart-container"></div>
    <script type="text/javascript">
        var dom = document.getElementById('chart-container');
        var myChart = echarts.init(dom, 'dark');
        var dates = {:?};
        var pnls = {:?};
        var drawdowns = {:?};

        var formattedDates = dates.map(ts => new Date(ts).toLocaleTimeString());

        var option = {{
            backgroundColor: '#1a1a1a',
            tooltip: {{ trigger: 'axis' }},
            legend: {{ data: ['Cumulative PnL', 'Drawdown %'] }},
            grid: {{ left: '3%', right: '4%', bottom: '3%', containLabel: true }},
            xAxis: {{ type: 'category', boundaryGap: false, data: formattedDates }},
            yAxis: [
                {{ type: 'value', name: 'PnL ($)', position: 'left', axisLabel: {{ formatter: '${{value}}' }} }},
                {{ type: 'value', name: 'Drawdown (%)', position: 'right', axisLabel: {{ formatter: '{{value}}%' }}, max: 100 }}
            ],
            series: [
                {{
                    name: 'Cumulative PnL',
                    type: 'line',
                    smooth: true,
                    data: pnls,
                    lineStyle: {{ color: '#00e676', width: 3 }},
                    areaStyle: {{ color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [{{ offset: 0, color: 'rgba(0, 230, 118, 0.3)' }}, {{ offset: 1, color: 'rgba(0, 230, 118, 0.05)' }}]) }}
                }},
                {{
                    name: 'Drawdown %',
                    type: 'line',
                    yAxisIndex: 1,
                    data: drawdowns,
                    lineStyle: {{ color: '#ff1744', width: 1, type: 'dashed' }},
                    areaStyle: {{ color: 'rgba(255, 23, 68, 0.1)' }}
                }}
            ]
        }};
        myChart.setOption(option);
    </script>
</body>
</html>
"#,
            dates, pnls, drawdowns
        );

        let path = Path::new(PNL_CHART_FILE);

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("Failed to create chart directory: {}", e));
        }

        let _ = fs::write(path, html_content)
            .map_err(|e| anyhow::anyhow!("Failed to write PnL chart file: {}", e));
    }

    fn save_pnl_snapshot(&self) {
        let current_capital = self.risk_manager.current_capital;
        let initial_capital = self.risk_manager.initial_capital;
        let drawdown = if initial_capital > 0.0 {
            ((initial_capital - current_capital) / initial_capital * 100.0).max(0.0)
        } else {
            0.0
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(PNL_HISTORY_FILE)
        {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Failed to open PNL history file: {e}");
                return;
            }
        };

        if let Err(e) = writeln!(
            file,
            "{},{:.4},{:.4},{:.2}",
            now, self.session_pnl, self.cumulative_pnl, drawdown
        ) {
            eprintln!("Failed to write PNL snapshot: {e}");
            return;
        }

        self.generate_pnl_chart_html();
    }

    pub fn generate_daily_summary(&self) -> String {
        if self.daily_trades.is_empty() {
            return "<b>Daily Summary</b>\nNo trades today.".to_string();
        }

        let total_pnl = self.session_pnl;
        let wins = self.daily_trades.iter().filter(|&&p| p > 0.0).count();
        let losses = self.daily_trades.len() - wins;
        let win_rate = (wins as f64 / self.daily_trades.len() as f64) * 100.0;

        let current_capital = self.risk_manager.current_capital;
        let initial_capital = self.risk_manager.initial_capital;
        let drawdown = if initial_capital > 0.0 {
            ((initial_capital - current_capital) / initial_capital * 100.0).max(0.0)
        } else {
            0.0
        };

        format!(
            "<b>Daily Summary Report</b>\n\n\
        Trades: {} ({}W / {}L)\n\
        Win Rate: {:.0}%\n\n\
        Cumulative PnL: ${:+.2}\n\
        Drawdown: {:.1}%\n\
        Capital: ${:.2}",
            self.daily_trades.len(),
            wins,
            losses,
            win_rate,
            total_pnl,
            drawdown,
            current_capital
        )
    }

    fn record_error(&mut self) {
        self.error_timestamps.push_back(Instant::now());
    }

    fn get_error_count_last_hour(&mut self) -> usize {
        let hour_ago = Instant::now() - Duration::from_secs(3600);
        while let Some(timestamp) = self.error_timestamps.front() {
            if *timestamp < hour_ago {
                self.error_timestamps.pop_front();
            } else {
                break;
            }
        }
        self.error_timestamps.len()
    }

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
            let req = TransactionRequest {
                to: *token_addr,
                from: Some(address),
                data: call.abi_encode().into(),
                nonce: None,
                gas_limit: None,
                max_fee_per_gas: None,
                max_priority_fee: None,
                chain_id: 42161,
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
                Err(e) => warn!("[Sync] Failed to fetch balance for {}: {:?}", symbol, e),
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
            let free = balance.free.to_human();
            let locked = balance.locked.to_human();
            if free.is_zero() && locked.is_zero() {
                continue;
            }
            let internal = if asset == "ETH" {
                "WETH".to_string()
            } else {
                asset.clone()
            };
            mapped_balances.insert(internal, (free, locked));
        }
        let mut inventory = self.shared_inventory.lock().await;
        inventory.update_from_cex(Venue::Cex, mapped_balances);
        Ok(())
    }

    pub async fn run(&mut self) {
        self.running = true;
        self.trading_active = true;

        if self.dry_run {
            info!("DRY RUN MODE ENABLED - No trades will be executed");
        }
        info!("Bot starting...");
        self.telegram.send("🚀 Arb Bot Started", false).await;

        let mut last_reset = Instant::now();
        let mut last_health_check = Instant::now();
        let mut last_kill_switch_check = Instant::now();

        while self.running {
            self.update_heartbeat();

            if last_kill_switch_check.elapsed() >= Duration::from_secs(10) {
                let kill_active = is_kill_switch_active();
                let error_count = self.get_error_count_last_hour();

                if self.trading_active {
                    if kill_active {
                        warn!("KILL SWITCH FILE DETECTED. Pausing Trading.");
                        self.telegram
                            .send(
                                "🛑 Manual Kill Switch (File) Detected! Flattening & Pausing.",
                                true,
                            )
                            .await;
                        self.emergency_flatten().await;
                        self.trading_active = false;
                    } else if self.telegram.check_for_stop_command().await {
                        warn!("Remote Stop Command Received via Telegram. Pausing Trading.");
                        self.emergency_flatten().await;
                        self.trading_active = false;
                    } else if self
                        .kill_switch
                        .check(&self.risk_manager, error_count, &self.telegram)
                        .await
                    {
                        warn!("AUTO KILL SWITCH TRIGGERED. Pausing Trading.");
                        self.emergency_flatten().await;
                        self.trading_active = false;
                    }
                } else if !kill_active
                    && !self.telegram.check_for_stop_command().await
                    && error_count == 0
                    && self.risk_manager.daily_pnl > -self.risk_manager.limits.max_daily_loss
                {
                    info!("Kill switch conditions cleared. Resuming trading...");
                    self.telegram
                        .send("▶️ <b>Kill switch cleared. Bot resuming.</b>", false)
                        .await;
                    self.trading_active = true;
                }
                last_kill_switch_check = Instant::now();
            }

            if last_reset.elapsed().as_secs() >= 86400 {
                self.risk_manager.reset_daily();
                let summary = self.generate_daily_summary();
                self.telegram.send(&summary, false).await;
                self.session_pnl = 0.0;
                self.daily_trades.clear();
                last_reset = Instant::now();
            }

            if last_health_check.elapsed() >= Duration::from_secs(60) {
                self.save_pnl_snapshot();

                self.update_gas_oracle().await;

                if let Err(e) = self.verify_balances().await {
                    error!("Integrity check failed: {}. Pausing trading for safety.", e);
                    self.trading_active = false;
                }

                let wins = self.daily_trades.iter().filter(|&&p| p > 0.0).count();
                let losses = self.daily_trades.len() - wins;
                let initial_capital = self.risk_manager.initial_capital;
                let current_capital = self.risk_manager.current_capital;
                let drawdown = if initial_capital > 0.0 {
                    ((initial_capital - current_capital) / initial_capital * 100.0).max(0.0)
                } else {
                    0.0
                };

                let state_payload = json!({
                    "trading_active": self.trading_active,
                    "dry_run": self.dry_run,
                    "session_pnl": self.session_pnl,
                    "cumulative_pnl": self.cumulative_pnl,
                    "capital": current_capital,
                    "drawdown": drawdown,
                    "trades_today": self.daily_trades.len(),
                    "wins": wins,
                    "losses": losses,
                    "uptime_secs": self.start_time.elapsed().as_secs()
                });

                self.telegram.update_bot_state(state_payload).await;

                let health = monitoring::BotHealth {
                    is_running: self.trading_active,
                    uptime_seconds: self.start_time.elapsed().as_secs(),
                    last_trade_age_seconds: monitoring::seconds_since(self.last_trade_time),
                    circuit_breaker_open: self.executor.is_circuit_open().await,
                    session_pnl: self.session_pnl,
                    daily_pnl_limit_reached: false,
                };
                health.log();
                last_health_check = Instant::now();
            }

            if let Err(e) = self.sync_binance_balances().await {
                self.record_error();
                monitoring::log_error("sync_binance", &e.to_string());
            }
            if let Err(e) = self.sync_wallet_balances().await {
                self.record_error();
                monitoring::log_error("sync_wallet", &e.to_string());
            }

            if self.trading_active {
                if let Err(e) = self.tick().await {
                    self.record_error();
                    monitoring::log_error("tick", &e.to_string());
                    sleep(Duration::from_secs(5)).await;
                } else {
                    sleep(Duration::from_secs(1)).await;
                }
            } else {
                sleep(Duration::from_secs(3)).await;
            }
        }

        let shutdown_summary = self.generate_daily_summary();
        self.telegram
            .send(
                &format!("🏁 <b>Bot Stopping</b>\n\n{}", shutdown_summary),
                false,
            )
            .await;
        self.telegram.send("🏁 Arb Bot Stopped", false).await;
    }

    async fn tick(&mut self) -> Result<()> {
        if self.executor.is_circuit_open().await {
            info!("Circuit breaker open - skipping tick");
            return Ok(());
        }

        self.generator.clear_queue();

        for pair in &self.pairs {
            let pair_cfg = self
                .pair_configs
                .get(pair)
                .cloned()
                .unwrap_or_else(PerPairConfig::pepe_default);

            self.generator.update_config(GeneratorConfig {
                min_spread_bps: pair_cfg.min_spread_bps,
                min_profit_usd: pair_cfg.min_profit_usd,
                ..Default::default()
            });

            self.generator
                .queue_candidate(pair, pair_cfg.trade_size)
                .await;
        }

        let mut candidates = Vec::new();
        while let Some(signal) = self.generator.pop_top_signal() {
            candidates.push(signal);
        }

        if candidates.is_empty() {
            return Ok(());
        }

        for signal in &mut candidates {
            let new_score = self.scorer.score(signal, &self.shared_inventory).await;
            signal.score = new_score;
            info!(
                "   Candidate: {} | Spread: {:.2} bps | Exp PnL: ${:.2} | Score: {:.2}",
                signal.pair, signal.spread_bps, signal.expected_net_pnl, signal.score
            );
        }

        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some(best_signal) = candidates.first() {
            let (valid_signal, reason) = self.validator.validate_signal(best_signal);
            if !valid_signal {
                warn!(
                    "VALIDATION FAILED (Signal): {} | Reason: {}",
                    best_signal.pair, reason
                );
                return Ok(());
            }

            let (valid_feed, feed_reason) = self
                .validator
                .validate_price_feed(best_signal.cex_price, &best_signal.pair);
            if !valid_feed {
                warn!(
                    "VALIDATION FAILED (Price Feed): {} | Reason: {}",
                    best_signal.pair, feed_reason
                );
                self.telegram
                    .send(
                        &format!(
                            "Price feed anomaly for {}: {}",
                            best_signal.pair, feed_reason
                        ),
                        true,
                    )
                    .await;
                self.record_error();
                return Ok(());
            }

            let trade_val_usd = best_signal.size * best_signal.cex_price;
            info!(
                "Checking Risk: Size={:.4}  | Value=${:.2} | Max Allowed=${:.2}",
                best_signal.size, trade_val_usd, self.risk_manager.limits.max_trade_usd
            );

            let (allowed, reason) = self.risk_manager.check_pre_trade(best_signal);
            if !allowed {
                warn!("RISK BLOCKED: {} | Reason: {}", best_signal.pair, reason);
                if reason.contains("Daily loss limit") {
                    self.kill_switch
                        .trigger(
                            &format!("Daily loss limit reached: {}", reason),
                            &self.telegram,
                        )
                        .await;
                    self.emergency_flatten().await;
                    self.trading_active = false;
                }
                return Ok(());
            }

            {
                let pair_cfg = self
                    .pair_configs
                    .get(&best_signal.pair)
                    .cloned()
                    .unwrap_or_else(PerPairConfig::pepe_default);
                let gross_profit =
                    best_signal.expected_net_pnl + pair_cfg.fee_structure.gas_cost_usd;
                if let Err(reason) =
                    safety_check_with_gas(gross_profit, pair_cfg.gas_units, &self.gas_oracle)
                {
                    warn!("GAS CHECK BLOCKED: {} | {}", best_signal.pair, reason);
                    return Ok(());
                }
            }

            info!(
                "Executing Signal: {} | Spread: {:.1} bps | Est PnL: ${:.2}",
                best_signal.pair, best_signal.spread_bps, best_signal.expected_net_pnl
            );

            if self.dry_run {
                info!(
                    "DRY RUN | Executing Trade Simulation: {}{:?} size={:.4}",
                    best_signal.pair, best_signal.direction, best_signal.size
                );
                return Ok(());
            }

            let signal_to_execute = best_signal.clone();
            let trade_usd = best_signal.size * best_signal.cex_price;
            let daily_loss = self.risk_manager.daily_pnl;
            let total_capital = self.risk_manager.current_capital;
            let trades_this_hour = self.risk_manager.trades_this_hour;

            if let Err(reason) = safety_check(
                trade_usd,
                daily_loss,
                total_capital,
                trades_this_hour.try_into().unwrap(),
            ) {
                warn!("ABSOLUTE SAFETY BLOCKED: {}", reason);
                self.kill_switch
                    .trigger(
                        &format!("Absolute safety triggered: {}", reason),
                        &self.telegram,
                    )
                    .await;
                self.emergency_flatten().await;
                self.trading_active = false;
                return Ok(());
            }

            let ctx = self.executor.execute(signal_to_execute.clone()).await;
            let success = ctx.state == ExecutorState::Done;

            self.scorer
                .record_result(signal_to_execute.pair.clone(), success);

            let pnl = ctx.actual_net_pnl.unwrap_or(0.0);
            self.risk_manager.record_trade(pnl);
            self.session_pnl += pnl;
            self.cumulative_pnl += pnl;
            self.daily_trades.push(pnl);
            self.save_pnl_snapshot();

            if success {
                self.last_trade_time = Some(Instant::now());
                let trade_msg = format!(
                    "✅ <b>Trade Completed</b>\nPair: {}\nPnL: ${:.2}\nSpread: {:.1}bps",
                    best_signal.pair, pnl, best_signal.spread_bps
                );
                self.telegram.send(&trade_msg, false).await;
            } else {
                self.record_error();
                if ctx.error.as_deref() == Some("Circuit breaker open") {
                    self.telegram
                        .send("⛔ Circuit breaker tripped! Halting trades.", true)
                        .await;
                }
            }

            monitoring::log_trade(
                &best_signal.pair,
                &format!("{:?}", best_signal.direction),
                best_signal.size,
                best_signal.spread_bps,
                pnl,
                &format!("{:?}", ctx.state),
            );

            if !success {
                monitoring::log_error(
                    "execution_failure",
                    ctx.error.as_deref().unwrap_or("Unknown execution error"),
                );
            }
        }
        Ok(())
    }

    async fn update_gas_oracle(&mut self) {
        match self.chain_client.get_gas_price().await {
            Ok(gas_price) => {
                let base_fee_gwei = gas_price.base_fee as f64 / 1e9;
                self.gas_oracle.update(base_fee_gwei);
            }
            Err(e) => {
                warn!("GasOracle: failed to fetch base fee, EWM unchanged: {}", e);
            }
        }

        if let Some(eth_price) = self.generator.last_price("WETH/USDT") {
            self.gas_oracle.set_eth_price(eth_price);
        }

        info!(
            "GasOracle | base fee EWM: {:.4} gwei | pessimistic (+1.5σ): {:.4} gwei",
            self.gas_oracle.current_gwei(),
            self.gas_oracle.pessimistic_estimate(),
        );
    }

    async fn verify_balances(&mut self) -> Result<()> {
        info!("Watchdog: Full balance reconciliation (CEX & DEX)...");

        let actual_cex_map = self.exchange.fetch_balance().await?;
        let eth_balance_raw = self.chain_client.get_balance(self.wallet.address()).await?;
        let actual_eth_dex = eth_balance_raw.to_human();

        let inventory = self.shared_inventory.lock().await;

        let expected_eth_dex = inventory.get_balance(Venue::Wallet, "ETH");
        let eth_dex_diff = (actual_eth_dex - expected_eth_dex).abs();

        let threshold = rust_decimal::Decimal::from_f64(0.001).unwrap();

        if eth_dex_diff > threshold {
            let msg = format!(
                "DEX BALANCE MISMATCH! ETH Actual: {}, Expected: {}, Diff: {}",
                actual_eth_dex, expected_eth_dex, eth_dex_diff
            );
            error!("{}", msg);
            self.kill_switch
                .trigger("DEX Integrity Failure", &self.telegram)
                .await;
            self.emergency_flatten().await;
            self.trading_active = false;
        }

        for pair in &self.pairs {
            let parts: Vec<&str> = pair.split('/').collect();
            if parts.len() == 2 {
                let base = parts[0];

                let actual_cex = actual_cex_map
                    .get(base)
                    .map(|b| b.free.to_human())
                    .unwrap_or(rust_decimal::Decimal::ZERO);
                let expected_cex = inventory.get_balance(Venue::Cex, base);

                let cex_diff = (actual_cex - expected_cex).abs();
                if cex_diff > threshold {
                    error!(
                        "CEX MISMATCH! {} Actual: {}, Expected: {}",
                        base, actual_cex, expected_cex
                    );
                    self.kill_switch
                        .trigger("CEX Integrity Failure", &self.telegram)
                        .await;
                    self.emergency_flatten().await;
                    self.trading_active = false;
                }
            }
        }

        Ok(())
    }
}

struct BotConfig {
    pairs: Vec<String>,
    simulation: bool,
    dry_run: bool,
    webhook_url: Option<String>,
    dex_router: Option<String>,
    dex_pools: HashMap<String, String>,
    token_addresses: HashMap<String, Address>,
    pair_configs: HashMap<String, PerPairConfig>,
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Err(e) = monitoring::setup_logger() {
        eprintln!("Failed to initialize logger: {}", e);
        return Ok(());
    }

    dotenvy::dotenv().ok();

    tokio::spawn(async move {
        let metrics_route = warp::path("metrics").map(gather_metrics).boxed();
        info!("Metrics server starting on http://localhost:3030/metrics");
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], 3030).into();
        warp::serve(metrics_route).run(addr).await;
    });

    info!("=== Configuring Bot for Multiple Pairs (PEPE & ETH) ===");

    let mut token_addresses = HashMap::new();

    let pepe_addr = env::var("PEPE_TOKEN")
        .unwrap_or_else(|_| "0x25d887Ce7a35172C62FeBFD67a1856F20FaEbB00".to_string());
    token_addresses.insert("PEPE".to_string(), Address::from_string(&pepe_addr)?);

    let usdt_addr = env::var("ARBITRUM_USDT")
        .unwrap_or_else(|_| "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9".to_string());
    token_addresses.insert("USDT".to_string(), Address::from_string(&usdt_addr)?);

    let weth_addr = env::var("ARBITRUM_WETH")
        .unwrap_or_else(|_| "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".to_string());
    token_addresses.insert("WETH".to_string(), Address::from_string(&weth_addr)?);

    let mut dex_pools = HashMap::new();

    dex_pools.insert(
        "PEPE/USDT".to_string(),
        env::var("PEPE_USDT_POOL")
            .unwrap_or_else(|_| "0xcd518702e7d8f16d8a6407bf5d22e0f0eec15162".to_string()),
    );

    dex_pools.insert(
        "WETH/USDT".to_string(),
        "0x641c00a822e8b671738d32a431a4fb6074e5c79d".to_string(),
    );

    let mut pair_configs = HashMap::new();
    pair_configs.insert("PEPE/USDT".to_string(), PerPairConfig::pepe_default());
    pair_configs.insert("WETH/USDT".to_string(), PerPairConfig::eth_default());

    let config = BotConfig {
        pairs: vec!["PEPE/USDT".to_string(), "WETH/USDT".to_string()],
        simulation: false,
        dry_run: false,
        webhook_url: None,
        dex_router: Some(
            env::var("ARBITRUM_ROUTER")
                .unwrap_or_else(|_| "0xE592427A0AEce92De3Edee1F18E0157C05861564".to_string()),
        ),
        dex_pools,
        token_addresses,
        pair_configs,
    };

    match ArbBot::new(config).await {
        Ok(mut bot) => {
            info!("Bot initialized. Heartbeat file: {}", HEARTBEAT_FILE);
            info!("PnL History file: {}", PNL_HISTORY_FILE);
            info!("PnL Dashboard: {}", PNL_CHART_FILE);
            bot.run().await;
        }
        Err(e) => error!("Bot initialization failed: {}", e),
    }
    Ok(())
}
