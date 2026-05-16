pub const ABSOLUTE_MAX_TRADE_USD: f64 = 25.0;
pub const ABSOLUTE_MAX_DAILY_LOSS: f64 = 20.0;
pub const ABSOLUTE_MIN_CAPITAL: f64 = 50.0;
pub const ABSOLUTE_MAX_TRADES_PER_HOUR: u32 = 30;

pub fn safety_check(
    trade_usd: f64,
    daily_loss: f64,
    total_capital: f64,
    trades_this_hour: u32,
) -> Result<(), String> {
    if trade_usd > ABSOLUTE_MAX_TRADE_USD {
        return Err(format!("Trade ${:.0} exceeds absolute max", trade_usd));
    }

    if daily_loss <= -ABSOLUTE_MAX_DAILY_LOSS {
        return Err("Absolute daily loss limit reached".to_string());
    }

    if total_capital < ABSOLUTE_MIN_CAPITAL {
        return Err(format!("Capital ${:.0} below minimum", total_capital));
    }

    if trades_this_hour >= ABSOLUTE_MAX_TRADES_PER_HOUR {
        return Err("Absolute hourly trade limit reached".to_string());
    }

    Ok(())
}
