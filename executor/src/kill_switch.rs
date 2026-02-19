use crate::monitoring;
use crate::position_limits::RiskManager;
use crate::telegram_alert::TelegramAlert;
use log::warn;
use std::sync::Arc;

/// Automatic safety switch that shuts down the bot based on catastrophic conditions.
pub struct AutoKillSwitch {
    pub triggered: bool,
    pub reason: Option<String>,
}

impl AutoKillSwitch {
    pub fn new() -> Self {
        Self {
            triggered: false,
            reason: None,
        }
    }

    /// Checks for critical conditions that should shut down the bot immediately.
    /// Returns true if the bot was triggered (killed).
    pub async fn check(
        &mut self,
        risk_manager: &RiskManager,
        error_count_last_hour: usize,
        telegram: &Arc<TelegramAlert>,
    ) -> bool {
        if self.triggered {
            return true;
        }

        if risk_manager.current_capital < risk_manager.initial_capital * 0.5 {
            self.trigger("Capital dropped below 50% of initial", telegram)
                .await;
            return true;
        }

        if risk_manager.daily_pnl <= -risk_manager.limits.max_daily_loss {
            self.trigger("Daily loss limit reached", telegram).await;
            return true;
        }

        let drawdown = if risk_manager.peak_capital > 0.0 {
            (risk_manager.peak_capital - risk_manager.current_capital) / risk_manager.peak_capital
        } else {
            0.0
        };
        if drawdown >= risk_manager.limits.max_drawdown_pct {
            self.trigger(
                &format!("Drawdown {:.1}% exceeds limit", drawdown * 100.0),
                telegram,
            )
            .await;
            return true;
        }

        if risk_manager.consecutive_losses >= risk_manager.limits.consecutive_loss_limit {
            self.trigger(
                &format!(
                    "Consecutive loss limit ({}) reached",
                    risk_manager.limits.consecutive_loss_limit
                ),
                telegram,
            )
            .await;
            return true;
        }

        if risk_manager.trades_this_hour >= risk_manager.limits.max_trades_per_hour {
            self.trigger(
                &format!(
                    "Hourly trade limit ({}) reached",
                    risk_manager.limits.max_trades_per_hour
                ),
                telegram,
            )
            .await;
            return true;
        }

        if error_count_last_hour > 50 {
            self.trigger("Critical error frequency (>50/hour)", telegram)
                .await;
            return true;
        }

        false
    }

    /// Explicitly triggers the kill switch with a reason.
    pub async fn trigger(&mut self, reason: &str, telegram: &Arc<TelegramAlert>) {
        if self.triggered {
            return;
        }

        self.triggered = true;
        self.reason = Some(reason.to_string());

        monitoring::log_error("KILL_SWITCH_TRIGGERED", reason);
        warn!("CRITICAL: Auto kill switch triggered: {}", reason);

        telegram
            .send(&format!("🚨 <b>BOT KILLED</b>: {}", reason), true)
            .await;
    }
}

impl Default for AutoKillSwitch {
    fn default() -> Self {
        Self::new()
    }
}
