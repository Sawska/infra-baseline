//! File-backed strategy configuration.
//!
//! All of the tunable "knobs" that used to live as hard-coded literals inside the bot
//! binary (spread/profit thresholds, trade sizes, fee assumptions, risk limits, gas
//! parameters, starting capital) are expressed here and loaded from `strategy.yml` at
//! startup. The path can be overridden with the `STRATEGY_CONFIG_PATH` env var.
//!
//! Behaviour on load:
//! * file missing            -> built-in defaults (back-compatible with the old literals)
//! * file present but invalid -> hard error (never trade with a half-parsed config)

use anyhow::{Context, Result};
use executor::gas_oracle::gas_units;
use executor::position_limits::RiskLimits;
use serde::{Deserialize, Serialize};
use std::path::Path;
use strategy::fees::FeeStructure;
use strategy::generator::GeneratorConfig;
use strategy::scorer::ScorerConfig;

pub const DEFAULT_PATH: &str = "strategy.yml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StrategyConfig {
    pub capital: CapitalConfig,
    pub generator: GeneratorConfig,
    pub scorer: ScorerConfig,
    pub risk_limits: RiskLimits,
    pub gas: GasConfig,
    pub balance_snapshot: SnapshotConfig,
    pub pairs: Vec<PairStrategy>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            capital: CapitalConfig::default(),
            generator: GeneratorConfig {
                min_spread_bps: 50.0,
                min_profit_usd: 0.01,
                ..Default::default()
            },
            scorer: ScorerConfig::default(),
            risk_limits: RiskLimits {
                max_trade_usd: 10.0,
                max_trade_pct: 0.20,
                max_position_per_token: 10.0,
                max_open_positions: 1,
                max_drawdown_pct: 0.20,
                max_daily_loss: 10.0,
                max_trades_per_hour: 20,
                consecutive_loss_limit: 3,
                max_loss_per_trade: 5.0,
            },
            gas: GasConfig::default(),
            balance_snapshot: SnapshotConfig::default(),
            pairs: vec![PairStrategy::pepe_default(), PairStrategy::eth_default()],
        }
    }
}

impl StrategyConfig {
    /// Load the strategy config, honouring `STRATEGY_CONFIG_PATH` (default `strategy.yml`).
    pub fn load() -> Result<Self> {
        let path =
            std::env::var("STRATEGY_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_PATH.to_string());
        Self::load_from(path)
    }

    pub fn load_from<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            log::warn!(
                "Strategy config '{}' not found — using built-in defaults.",
                path.display()
            );
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading strategy config '{}'", path.display()))?;
        let cfg: StrategyConfig = serde_yaml::from_str(&raw)
            .with_context(|| format!("parsing strategy config '{}'", path.display()))?;
        log::info!("Loaded strategy config from '{}'.", path.display());
        Ok(cfg)
    }

    pub fn pair(&self, symbol: &str) -> Option<&PairStrategy> {
        self.pairs.iter().find(|p| p.symbol == symbol)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CapitalConfig {
    /// Fixed starting capital in USD. When omitted (`null`), capital is derived from live
    /// CEX + on-chain wallet balances at startup ("live balance fetching").
    pub initial_capital_usd: Option<f64>,
    /// Used only when live derivation yields nothing usable (e.g. price feeds unavailable).
    pub fallback_capital_usd: f64,
}

impl Default for CapitalConfig {
    fn default() -> Self {
        Self {
            initial_capital_usd: None,
            fallback_capital_usd: 71.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GasConfig {
    pub initial_gwei: f64,
    pub alpha: f64,
    pub eth_price_usd: f64,
}

impl Default for GasConfig {
    fn default() -> Self {
        Self {
            initial_gwei: 0.1,
            alpha: 0.1,
            eth_price_usd: 3_000.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotConfig {
    pub enabled: bool,
    pub interval_secs: u64,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 60,
        }
    }
}

/// Symbolic gas-unit estimate for a pair's execution path; maps onto the constants in
/// [`executor::gas_oracle::gas_units`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GasUnitsKind {
    Erc20Transfer,
    UniswapV2Swap,
    UniswapV3Swap,
    TwoHopV3,
    FlashLoanArb,
}

impl GasUnitsKind {
    pub fn units(self) -> u64 {
        match self {
            GasUnitsKind::Erc20Transfer => gas_units::ERC20_TRANSFER,
            GasUnitsKind::UniswapV2Swap => gas_units::UNISWAP_V2_SWAP,
            GasUnitsKind::UniswapV3Swap => gas_units::UNISWAP_V3_SWAP,
            GasUnitsKind::TwoHopV3 => gas_units::TWO_HOP_V3,
            GasUnitsKind::FlashLoanArb => gas_units::FLASH_LOAN_ARB,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PairStrategy {
    pub symbol: String,
    pub trade_size: f64,
    pub min_spread_bps: f64,
    pub min_profit_usd: f64,
    pub gas_units: GasUnitsKind,
    pub fees: FeeStructure,
}

impl Default for PairStrategy {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            trade_size: 0.0,
            min_spread_bps: 50.0,
            min_profit_usd: 0.01,
            gas_units: GasUnitsKind::UniswapV3Swap,
            fees: FeeStructure::default(),
        }
    }
}

impl PairStrategy {
    pub fn pepe_default() -> Self {
        Self {
            symbol: "PEPE/USDT".to_string(),
            trade_size: 1_250_000.0,
            min_spread_bps: 130.0,
            min_profit_usd: 0.01,
            gas_units: GasUnitsKind::TwoHopV3,
            fees: FeeStructure {
                cex_taker_bps: 10.0,
                dex_swap_bps: 1.0,
                gas_cost_usd: 0.07,
            },
        }
    }

    pub fn eth_default() -> Self {
        Self {
            symbol: "WETH/USDT".to_string(),
            trade_size: 0.0026,
            min_spread_bps: 20.0,
            min_profit_usd: 0.02,
            gas_units: GasUnitsKind::UniswapV3Swap,
            fees: FeeStructure {
                cex_taker_bps: 10.0,
                dex_swap_bps: 5.0,
                gas_cost_usd: 0.02,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_strategy_yml_parses() {
        let raw = include_str!("../../strategy.yml");
        let cfg: StrategyConfig = serde_yaml::from_str(raw).expect("strategy.yml must parse");
        assert_eq!(cfg.pairs.len(), 2);
        let pepe = cfg.pair("PEPE/USDT").expect("PEPE pair present");
        assert_eq!(pepe.gas_units.units(), gas_units::TWO_HOP_V3);
        assert_eq!(pepe.trade_size, 1_250_000.0);
        // Default behaviour: derive capital from live balances.
        assert!(cfg.capital.initial_capital_usd.is_none());
        assert!(cfg.balance_snapshot.enabled);
    }

    #[test]
    fn empty_yaml_falls_back_to_defaults() {
        let cfg: StrategyConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(cfg.pairs.len(), 2);
        assert_eq!(cfg.capital.fallback_capital_usd, 71.0);
        assert_eq!(cfg.gas.eth_price_usd, 3_000.0);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = StrategyConfig::load_from("/nonexistent/strategy.yml").unwrap();
        assert_eq!(cfg.pairs.len(), 2);
    }
}
