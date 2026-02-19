use anyhow::Result;
use chrono::Local;
use log::{error, info};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Configures logging to match Python specs:
/// 1. Console + File output
/// 2. Daily log files (logs/bot_YYYYMMDD.log)
/// 3. Specific format: timestamp | level | message
pub fn setup_logger() -> Result<()> {
    if !Path::new("logs").exists() {
        fs::create_dir("logs")?;
    }

    let now = Local::now();
    let filename = format!("logs/bot_{}.log", now.format("%Y%m%d"));

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} |{:5} |{}",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Info)
        .chain(std::io::stdout())
        .chain(fern::log_file(filename)?)
        .apply()?;

    Ok(())
}

/// Metrics that must be healthy for trading.
/// Checks critical components like connectivity, capital, and pnl.
#[derive(Debug, Serialize)]
pub struct BotHealth {
    pub is_running: bool,
    pub uptime_seconds: u64,
    pub last_trade_age_seconds: Option<u64>,
    pub circuit_breaker_open: bool,
    pub session_pnl: f64,
    pub daily_pnl_limit_reached: bool,
}

impl BotHealth {
    pub fn log(&self) {
        info!(
            "HEALTH | uptime={}s | last_trade={:?}s | cb_open={} | pnl=${:.2} | limit_reached={}",
            self.uptime_seconds,
            self.last_trade_age_seconds,
            self.circuit_breaker_open,
            self.session_pnl,
            self.daily_pnl_limit_reached
        );
    }
}

/// Structured logging for executed trades.
/// Used for parsing logs later to analyze strategy performance.
pub fn log_trade(pair: &str, direction: &str, size: f64, spread_bps: f64, pnl: f64, state: &str) {
    info!(
        "TRADE | pair={} | dir={} | size={:.4} | spread={:.1}bps | pnl=${:.2} | state={}",
        pair, direction, size, spread_bps, pnl, state
    );
}

/// Structured logging for errors with context.
pub fn log_error(context: &str, error: &str) {
    error!("ERROR | ctx={} | msg={}", context, error);
}

/// Helper to calculate age of a timestamp
pub fn seconds_since(instant: Option<Instant>) -> Option<u64> {
    instant.map(|t| t.elapsed().as_secs())
}
