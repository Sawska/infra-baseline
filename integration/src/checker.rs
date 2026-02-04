use anyhow::{Result, anyhow};
use arb_core::types::TokenAmount;
use chrono::{DateTime, Utc};
use exchange::client::ExchangeClient;
use inventory::pnl::PnLEngine;
use inventory::tracker::{InventoryTracker, Venue};
use pricing::amm::Token;
use pricing::engine::PricingEngine;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use std::future::Future;

pub struct ArbChecker<F> {
    pub pricing_engine: PricingEngine<F>,
    pub exchange_client: ExchangeClient,
    pub inventory_tracker: InventoryTracker,
    pub pnl_engine: PnLEngine,
}

#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub pair: String,
    pub timestamp: DateTime<Utc>,
    pub dex_price: Decimal,
    pub cex_bid: Decimal,
    pub cex_ask: Decimal,
    pub gap_bps: Decimal,
    pub direction: Option<String>,
    pub estimated_costs_bps: Decimal,
    pub estimated_net_pnl_bps: Decimal,
    pub inventory_ok: bool,
    pub executable: bool,
    pub details: ArbDetails,
}

#[derive(Debug, Clone)]
pub struct ArbDetails {
    pub dex_price_impact_bps: Decimal,
    pub cex_slippage_bps: Decimal,
    pub cex_fee_bps: Decimal,
    pub dex_fee_bps: Decimal,
    pub gas_cost_usd: Decimal,
}

impl<F, Fut> ArbChecker<F>
where
    F: Fn(pricing::monitor::MonitorEvent) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    pub fn new(
        pricing_engine: PricingEngine<F>,
        exchange_client: ExchangeClient,
        inventory_tracker: InventoryTracker,
        pnl_engine: PnLEngine,
    ) -> Self {
        Self {
            pricing_engine,
            exchange_client,
            inventory_tracker,
            pnl_engine,
        }
    }

    pub async fn check(
        &self,
        pair: &str,
        size: Decimal,
        token_in: &Token,
        token_out: &Token,
    ) -> Result<ArbOpportunity> {
        let symbol_cex = pair.replace("/", "").to_uppercase();

        let raw_amount_in = TokenAmount::from_human(
            &size.to_string(),
            token_in.decimals,
            Some(token_in.symbol.clone()),
        )?;

        let router = self
            .pricing_engine
            .router
            .as_ref()
            .ok_or_else(|| anyhow!("Pricing router not initialized"))?;

        let (_route, raw_output) = router
            .find_best_route(token_in, token_out, raw_amount_in.raw, 20, 3)
            .map_err(|e| anyhow!("Routing error: {:?}", e))?;

        let output_amount = TokenAmount {
            raw: raw_output,
            decimals: token_out.decimals,
            symbol: Some(token_out.symbol.clone()),
        };

        let dex_price = output_amount.to_human() / size;

        let ob = self
            .exchange_client
            .fetch_order_book(&symbol_cex, 5)
            .await
            .map_err(|e| anyhow!("Exchange error for {}: {:?}", symbol_cex, e))?;

        let cex_bid = ob.best_bid.0;
        let cex_ask = ob.best_ask.0;

        let gap_usd = dex_price - cex_ask;
        let gap_bps = (gap_usd / cex_ask) * dec!(10000);

        let dex_fee_bps = dec!(30.0);
        let cex_fee_bps = dec!(10.0);
        let dex_impact_bps = dec!(1.2);
        let cex_slippage_bps = dec!(0.4);
        let gas_cost_usd = dec!(5.0);

        let notional = size * dex_price;
        let gas_bps = if notional > Decimal::ZERO {
            (gas_cost_usd / notional) * dec!(10000)
        } else {
            Decimal::ZERO
        };

        let total_costs_bps =
            dex_fee_bps + cex_fee_bps + dex_impact_bps + cex_slippage_bps + gas_bps;
        let net_pnl_bps = gap_bps - total_costs_bps;

        let inv_check = self.inventory_tracker.can_execute(
            Venue::Wallet,
            &token_in.symbol,
            size,
            Venue::Binance,
            &token_out.symbol,
            size * cex_ask,
        );
        let inventory_ok = inv_check["can_execute"].as_bool().unwrap_or(false);

        Ok(ArbOpportunity {
            pair: pair.to_string(),
            timestamp: Utc::now(),
            dex_price,
            cex_bid,
            cex_ask,
            gap_bps,
            direction: Some("sell_dex_buy_cex".to_string()),
            estimated_costs_bps: total_costs_bps,
            estimated_net_pnl_bps: net_pnl_bps,
            inventory_ok,
            executable: inventory_ok && net_pnl_bps > dec!(0),
            details: ArbDetails {
                dex_price_impact_bps: dex_impact_bps,
                cex_slippage_bps,
                cex_fee_bps,
                dex_fee_bps,
                gas_cost_usd,
            },
        })
    }
}
