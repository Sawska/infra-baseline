use crate::tracker::Venue;
use chrono::{DateTime, Utc};
use csv::Writer;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::Write;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeLeg {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub venue: Venue,
    pub symbol: String,
    pub side: Side,
    pub amount: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    pub fee: Decimal,
    pub fee_asset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub buy_leg: TradeLeg,
    pub sell_leg: TradeLeg,
    pub gas_cost_usd: Decimal,
}

impl ArbRecord {
    pub fn notional(&self) -> Decimal {
        self.buy_leg.amount * self.buy_leg.price
    }

    pub fn total_fees(&self) -> Decimal {
        self.buy_leg.fee + self.sell_leg.fee + self.gas_cost_usd
    }

    pub fn gross_pnl(&self) -> Decimal {
        (self.sell_leg.price - self.buy_leg.price) * self.buy_leg.amount
    }

    pub fn net_pnl(&self) -> Decimal {
        self.gross_pnl() - self.total_fees()
    }

    pub fn net_pnl_bps(&self) -> Decimal {
        let notional = self.notional();
        if notional.is_zero() {
            return dec!(0);
        }
        (self.net_pnl() / notional) * dec!(10000)
    }
}

pub struct PnLEngine {
    pub pool: PgPool,
}

impl PnLEngine {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn record(&self, trade: ArbRecord) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO arb_trades (
                id, timestamp, symbol,
                buy_venue, buy_amount, buy_price, buy_fee, buy_fee_asset,
                sell_venue, sell_amount, sell_price, sell_fee, sell_fee_asset,
                gas_cost_usd, net_pnl, bps
            ) VALUES (
                $1, $2, $3,
                $4, $5, $6, $7, $8,
                $9, $10, $11, $12, $13,
                $14, $15, $16
            )
            "#,
        )
        .bind(&trade.id)
        .bind(trade.timestamp)
        .bind(&trade.buy_leg.symbol)
        .bind(format!("{:?}", trade.buy_leg.venue))
        .bind(trade.buy_leg.amount)
        .bind(trade.buy_leg.price)
        .bind(trade.buy_leg.fee)
        .bind(&trade.buy_leg.fee_asset)
        .bind(format!("{:?}", trade.sell_leg.venue))
        .bind(trade.sell_leg.amount)
        .bind(trade.sell_leg.price)
        .bind(trade.sell_leg.fee)
        .bind(&trade.sell_leg.fee_asset)
        .bind(trade.gas_cost_usd)
        .bind(trade.net_pnl())
        .bind(trade.net_pnl_bps())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Persist a point-in-time snapshot of account equity (live CEX + on-chain wallet
    /// balances valued in USD) so the equity curve can be charted alongside realised PnL.
    pub async fn record_balance_snapshot(
        &self,
        timestamp: DateTime<Utc>,
        cex_usd: Decimal,
        wallet_usd: Decimal,
        total_usd: Decimal,
        breakdown_json: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO balance_snapshots (timestamp, cex_usd, wallet_usd, total_usd, breakdown)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(timestamp)
        .bind(cex_usd)
        .bind(wallet_usd)
        .bind(total_usd)
        .bind(breakdown_json)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Most recent equity snapshot, if any: `(timestamp, cex_usd, wallet_usd, total_usd)`.
    pub async fn latest_balance_snapshot(
        &self,
    ) -> Result<Option<(DateTime<Utc>, Decimal, Decimal, Decimal)>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT timestamp, cex_usd, wallet_usd, total_usd \
             FROM balance_snapshots ORDER BY timestamp DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some((
                row.try_get("timestamp")?,
                row.try_get("cex_usd")?,
                row.try_get("wallet_usd")?,
                row.try_get("total_usd")?,
            ))),
            None => Ok(None),
        }
    }

    pub async fn fetch_all_trades(&self) -> Result<Vec<ArbRecord>, sqlx::Error> {
        let rows = sqlx::query("SELECT * FROM arb_trades ORDER BY timestamp ASC")
            .fetch_all(&self.pool)
            .await?;

        let mut trades = Vec::with_capacity(rows.len());

        for row in rows {
            let id: String = row.try_get("id")?;
            let timestamp: DateTime<Utc> = row.try_get("timestamp")?;
            let symbol: String = row.try_get("symbol")?;

            let buy_venue_str: String = row.try_get("buy_venue")?;
            let buy_venue = if buy_venue_str == "Cex" {
                Venue::Cex
            } else {
                Venue::Wallet
            };

            let sell_venue_str: String = row.try_get("sell_venue")?;
            let sell_venue = if sell_venue_str == "Cex" {
                Venue::Cex
            } else {
                Venue::Wallet
            };

            let buy_leg = TradeLeg {
                id: format!("{}-buy", id),
                timestamp,
                venue: buy_venue,
                symbol: symbol.clone(),
                side: Side::Buy,
                amount: row.try_get("buy_amount")?,
                price: row.try_get("buy_price")?,
                fee: row.try_get("buy_fee")?,
                fee_asset: row.try_get("buy_fee_asset")?,
            };

            let sell_leg = TradeLeg {
                id: format!("{}-sell", id),
                timestamp,
                venue: sell_venue,
                symbol,
                side: Side::Sell,
                amount: row.try_get("sell_amount")?,
                price: row.try_get("sell_price")?,
                fee: row.try_get("sell_fee")?,
                fee_asset: row.try_get("sell_fee_asset")?,
            };

            trades.push(ArbRecord {
                id,
                timestamp,
                buy_leg,
                sell_leg,
                gas_cost_usd: row.try_get("gas_cost_usd")?,
            });
        }

        Ok(trades)
    }

    pub async fn cumulative_pnl(&self) -> Result<Vec<(DateTime<Utc>, Decimal)>, sqlx::Error> {
        let trades = self.fetch_all_trades().await?;
        let mut cumulative = dec!(0);
        let series = trades
            .into_iter()
            .map(|t| {
                cumulative += t.net_pnl();
                (t.timestamp, cumulative)
            })
            .collect();
        Ok(series)
    }

    pub async fn summary(&self) -> Result<HashMap<String, String>, sqlx::Error> {
        let trades = self.fetch_all_trades().await?;
        let mut report = HashMap::new();

        if trades.is_empty() {
            return Ok(report);
        }

        let count = trades.len();
        let pnls: Vec<Decimal> = trades.iter().map(|t| t.net_pnl()).collect();

        let total_pnl: Decimal = pnls.iter().sum();
        let total_fees: Decimal = trades.iter().map(|t| t.total_fees()).sum();
        let total_notional: Decimal = trades.iter().map(|t| t.notional()).sum();

        let wins = pnls.iter().filter(|&&p| p > dec!(0)).count();
        let win_rate = (wins as f64 / count as f64) * 100.0;

        let avg_pnl = total_pnl / Decimal::from(count);
        let total_bps = trades.iter().map(|t| t.net_pnl_bps()).sum::<Decimal>();
        let avg_bps = total_bps / Decimal::from(count);

        let avg_f = avg_pnl.to_f64().unwrap_or(0.0);
        let variance = pnls
            .iter()
            .map(|p| (p.to_f64().unwrap_or(0.0) - avg_f).powi(2))
            .sum::<f64>()
            / count as f64;
        let std_dev = variance.sqrt();
        let sharpe = if std_dev > 0.0 { avg_f / std_dev } else { 0.0 };

        report.insert("total_trades".into(), count.to_string());
        report.insert("win_rate".into(), format!("{:.1}%", win_rate));
        report.insert("total_pnl_usd".into(), total_pnl.round_dp(2).to_string());
        report.insert("total_fees_usd".into(), total_fees.round_dp(2).to_string());
        report.insert("avg_pnl_per_trade".into(), avg_pnl.round_dp(2).to_string());
        report.insert("avg_pnl_bps".into(), avg_bps.round_dp(1).to_string());
        report.insert(
            "best_trade_pnl".into(),
            pnls.iter().max().cloned().unwrap_or(dec!(0)).to_string(),
        );
        report.insert(
            "worst_trade_pnl".into(),
            pnls.iter().min().cloned().unwrap_or(dec!(0)).to_string(),
        );
        report.insert(
            "total_notional".into(),
            total_notional.round_dp(0).to_string(),
        );
        report.insert("sharpe_estimate".into(), format!("{:.2}", sharpe));

        Ok(report)
    }

    pub async fn recent(&self, n: usize) -> Result<Vec<HashMap<String, String>>, sqlx::Error> {
        let trades = self.fetch_all_trades().await?;
        let result = trades
            .into_iter()
            .rev()
            .take(n)
            .map(|t| {
                let mut m = HashMap::new();
                m.insert("time".into(), t.timestamp.format("%H:%M").to_string());
                m.insert(
                    "asset".into(),
                    t.buy_leg
                        .symbol
                        .split('/')
                        .next()
                        .unwrap_or("?")
                        .to_string(),
                );
                m.insert(
                    "route".into(),
                    format!("Buy {:?} / Sell {:?}", t.buy_leg.venue, t.sell_leg.venue),
                );
                m.insert("pnl".into(), t.net_pnl().round_dp(2).to_string());
                m.insert("bps".into(), t.net_pnl_bps().round_dp(1).to_string());
                m.insert("is_win".into(), (t.net_pnl() > dec!(0)).to_string());
                m
            })
            .collect();
        Ok(result)
    }

    pub async fn export_plotly_html(&self, filepath: &str) -> Result<(), Box<dyn Error>> {
        let data = self.cumulative_pnl().await?;
        let x_values: Vec<String> = data.iter().map(|(ts, _)| ts.to_rfc3339()).collect();
        let y_values: Vec<f64> = data
            .iter()
            .map(|(_, pnl)| pnl.to_f64().unwrap_or(0.0))
            .collect();

        let x_json = serde_json::to_string(&x_values)?;
        let y_json = serde_json::to_string(&y_values)?;

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>PnL Historical Chart</title>
    <script src="https://cdn.plot.ly/plotly-latest.min.js"></script>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; margin: 40px; background: #f8fafc; color: #1e293b; }}
        .container {{ max-width: 1200px; margin: 0 auto; }}
        .header {{ margin-bottom: 24px; }}
        #chart {{ width: 100%; height: 600px; background: white; border-radius: 12px; box-shadow: 0 4px 6px -1px rgb(0 0 0 / 0.1); padding: 20px; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>Arbitrage Performance</h1>
            <p>Cumulative Net PnL (USD) over time</p>
        </div>
        <div id="chart"></div>
    </div>
    <script>
        const trace = {{
            x: {x_json},
            y: {y_json},
            type: 'scatter',
            mode: 'lines+markers',
            name: 'Cumulative PnL',
            line: {{ color: '#10b981', width: 3, shape: 'hv' }},
            marker: {{ color: '#059669', size: 6 }},
            fill: 'tozeroy',
            fillcolor: 'rgba(16, 185, 129, 0.1)'
        }};
        const layout = {{
            margin: {{ t: 10, r: 10, b: 40, l: 60 }},
            xaxis: {{ title: 'Time', gridcolor: '#f1f5f9', zeroline: false }},
            yaxis: {{ title: 'Net PnL (USD)', gridcolor: '#f1f5f9', tickprefix: '$' }},
            hovermode: 'x unified',
            paper_bgcolor: 'rgba(0,0,0,0)',
            plot_bgcolor: 'rgba(0,0,0,0)'
        }};
        Plotly.newPlot('chart', [trace], layout, {{ responsive: true }});
    </script>
</body>
</html>"#,
            x_json = x_json,
            y_json = y_json,
        );

        let mut file = File::create(filepath)?;
        file.write_all(html.as_bytes())?;
        Ok(())
    }

    pub async fn export_csv(&self, filepath: &str) -> Result<(), Box<dyn Error>> {
        let trades = self.fetch_all_trades().await?;
        let file = File::create(filepath)?;
        let mut wtr = Writer::from_writer(file);

        wtr.write_record([
            "id",
            "timestamp",
            "buy_venue",
            "sell_venue",
            "net_pnl",
            "bps",
        ])?;
        for t in &trades {
            wtr.write_record([
                &t.id,
                &t.timestamp.to_rfc3339(),
                &format!("{:?}", t.buy_leg.venue),
                &format!("{:?}", t.sell_leg.venue),
                &t.net_pnl().to_string(),
                &t.net_pnl_bps().to_string(),
            ])?;
        }
        wtr.flush()?;
        Ok(())
    }
}
