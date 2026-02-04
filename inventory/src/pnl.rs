use crate::tracker::Venue;
use chrono::{DateTime, Utc};
use csv::Writer;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
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
    pub trades: Vec<ArbRecord>,
}

impl Default for PnLEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PnLEngine {
    pub fn new() -> Self {
        Self { trades: Vec::new() }
    }

    pub fn record(&mut self, trade: ArbRecord) {
        self.trades.push(trade);
    }

    /// Calculates cumulative PnL over time for charting.
    pub fn cumulative_pnl(&self) -> Vec<(DateTime<Utc>, Decimal)> {
        let mut sorted_trades = self.trades.clone();
        sorted_trades.sort_by_key(|t| t.timestamp);

        let mut cumulative = dec!(0);
        sorted_trades
            .into_iter()
            .map(|t| {
                cumulative += t.net_pnl();
                (t.timestamp, cumulative)
            })
            .collect()
    }

    /// Exports an interactive Plotly.js chart as an HTML file.
    pub fn export_plotly_html(&self, filepath: &str) -> Result<(), Box<dyn Error>> {
        let data = self.cumulative_pnl();
        let x_values: Vec<String> = data.iter().map(|(ts, _)| ts.to_rfc3339()).collect();
        let y_values: Vec<f64> = data
            .iter()
            .map(|(_, pnl)| pnl.to_f64().unwrap_or(0.0))
            .collect();

        let x_json = serde_json::to_string(&x_values)?;
        let y_json = serde_json::to_string(&y_values)?;

        let html_content = format!(
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
            y_json = y_json
        );

        let mut file = File::create(filepath)?;
        file.write_all(html_content.as_bytes())?;
        Ok(())
    }

    pub fn summary(&self) -> HashMap<String, String> {
        let mut report = HashMap::new();
        if self.trades.is_empty() {
            return report;
        }

        let count = self.trades.len();
        let pnls: Vec<Decimal> = self.trades.iter().map(|t| t.net_pnl()).collect();

        let total_pnl: Decimal = pnls.iter().sum();
        let total_fees: Decimal = self.trades.iter().map(|t| t.total_fees()).sum();
        let total_notional: Decimal = self.trades.iter().map(|t| t.notional()).sum();

        let wins = pnls.iter().filter(|&&p| p > dec!(0)).count();
        let win_rate = (wins as f64 / count as f64) * 100.0;

        let avg_pnl = total_pnl / Decimal::from(count);
        let total_bps = self.trades.iter().map(|t| t.net_pnl_bps()).sum::<Decimal>();
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

        report
    }

    pub fn recent(&self, n: usize) -> Vec<HashMap<String, String>> {
        self.trades
            .iter()
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
            .collect()
    }

    pub fn export_csv(&self, filepath: &str) -> Result<(), Box<dyn Error>> {
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
        for t in &self.trades {
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
