use serde::{Deserialize, Serialize};
use strategy::signal::Signal;

/// Hard limits that cannot be exceeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskLimits {
    /// Never trade more than this USD amount per trade
    pub max_trade_usd: f64,
    /// Never trade more than this percentage of current capital per trade (0.0 - 1.0)
    pub max_trade_pct: f64,

    /// Position limits
    /// Never hold >$30 of any token (Not strictly enforced in check_pre_trade yet, requires inventory)
    pub max_position_per_token: f64,
    /// Only one trade at a time
    pub max_open_positions: usize,

    /// Loss limits
    /// Stop trading after this amount of daily loss
    pub max_daily_loss: f64,
    /// Stop trading if drawdown from peak capital exceeds this percentage (0.0 - 1.0)
    pub max_drawdown_pct: f64,
    /// Max allowable loss per single trade (e.g. max acceptable gas loss on revert)
    pub max_loss_per_trade: f64,

    /// Frequency limits
    /// Prevent runaway loops (max trades per hour)
    pub max_trades_per_hour: usize,
    /// Stop after N losses in a row
    pub consecutive_loss_limit: usize,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_trade_usd: 20.0,
            max_trade_pct: 0.20,
            max_position_per_token: 30.0,
            max_open_positions: 1,
            max_daily_loss: 15.0,
            max_drawdown_pct: 0.20,
            max_loss_per_trade: 5.0,
            max_trades_per_hour: 20,
            consecutive_loss_limit: 3,
        }
    }
}

/// Enforce risk limits before every trade and track state.
pub struct RiskManager {
    pub limits: RiskLimits,
    pub(crate) peak_capital: f64,
    pub current_capital: f64,
    pub daily_pnl: f64,
    pub trades_this_hour: usize,
    pub(crate) consecutive_losses: usize,
    pub initial_capital: f64,
}

impl RiskManager {
    pub fn new(limits: RiskLimits, initial_capital: f64) -> Self {
        Self {
            limits,
            peak_capital: initial_capital,
            current_capital: initial_capital,
            daily_pnl: 0.0,
            trades_this_hour: 0,
            consecutive_losses: 0,
            initial_capital,
        }
    }

    /// Check all risk limits before allowing a trade.
    /// Returns (allowed, reason).
    pub fn check_pre_trade(&self, signal: &Signal) -> (bool, String) {
        let trade_value = signal.size * signal.cex_price;

        if trade_value > self.limits.max_trade_usd {
            return (
                false,
                format!(
                    "Trade ${:.2} exceeds max ${:.2}",
                    trade_value, self.limits.max_trade_usd
                ),
            );
        }

        if trade_value > self.initial_capital * self.limits.max_trade_pct {
            return (
                false,
                format!(
                    "Trade ${:.2} exceeds {:.1}% of capital",
                    trade_value,
                    self.limits.max_trade_pct * 100.0
                ),
            );
        }

        if self.daily_pnl <= -self.limits.max_daily_loss {
            return (
                false,
                format!(
                    "Daily loss limit reached: ${:.2} (max ${:.2})",
                    self.daily_pnl, self.limits.max_daily_loss
                ),
            );
        }

        if self.peak_capital > 0.0 {
            let drawdown = (self.peak_capital - self.current_capital) / self.peak_capital;
            if drawdown >= self.limits.max_drawdown_pct {
                return (
                    false,
                    format!(
                        "Drawdown {:.1}% exceeds limit {:.1}%",
                        drawdown * 100.0,
                        self.limits.max_drawdown_pct * 100.0
                    ),
                );
            }
        }

        if self.consecutive_losses >= self.limits.consecutive_loss_limit {
            return (
                false,
                format!(
                    "Consecutive loss limit ({}) reached",
                    self.limits.consecutive_loss_limit
                ),
            );
        }

        if self.trades_this_hour >= self.limits.max_trades_per_hour {
            return (
                false,
                format!(
                    "Hourly trade limit reached ({})",
                    self.limits.max_trades_per_hour
                ),
            );
        }

        (true, "OK".to_string())
    }

    /// Update state after a trade execution.
    pub fn record_trade(&mut self, pnl: f64) {
        self.daily_pnl += pnl;
        self.current_capital += pnl;

        if self.current_capital > self.peak_capital {
            self.peak_capital = self.current_capital;
        }

        self.trades_this_hour += 1;

        if pnl < 0.0 {
            self.consecutive_losses += 1;

            if pnl < -self.limits.max_loss_per_trade {
                log::warn!(
                    "Trade loss ${:.2} exceeded max_loss_per_trade limit ${:.2}",
                    pnl,
                    self.limits.max_loss_per_trade
                );
            }
        } else {
            self.consecutive_losses = 0;
        }

        println!(
            "Record trade: pnl={}, daily_pnl={}, current_capital={}, peak={}, trades_this_hour={}, consecutive_losses={}",
            pnl,
            self.daily_pnl,
            self.current_capital,
            self.peak_capital,
            self.trades_this_hour,
            self.consecutive_losses
        );
    }

    /// Call at start of each trading day.
    pub fn reset_daily(&mut self) {
        self.daily_pnl = 0.0;
        self.trades_this_hour = 0;
        self.consecutive_losses = 0;
    }
}
