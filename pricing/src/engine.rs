#![allow(clippy::collapsible_if)]
use crate::amm::{Pool, Token};
use crate::monitor::{MempoolMonitor, MonitorEvent};
use crate::router::{Route, RouteFinder};
use crate::simulator::ForkSimulator;
use alloy_primitives::U256;
use anyhow::{Result, anyhow};
use arb_chain::ChainClient;
use arb_core::Address;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Quote {
    pub route: Route,
    pub amount_in: U256,
    pub expected_output: U256,
    pub simulated_output: U256,
    pub gas_estimate: u64,
    pub timestamp: u64,
}

impl Quote {
    pub fn is_valid(&self) -> bool {
        let diff = if self.expected_output > self.simulated_output {
            self.expected_output - self.simulated_output
        } else {
            self.simulated_output - self.expected_output
        };

        match diff.checked_mul(U256::from(1000)) {
            Some(scaled_diff) => scaled_diff < self.expected_output,
            None => false,
        }
    }
}

pub struct PricingEngine<F> {
    pub client: Arc<ChainClient>,
    pub simulator: ForkSimulator,
    pub monitor: MempoolMonitor<F>,
    pub pools: HashMap<Address, Pool>,
    pub router: Option<RouteFinder>,
}

impl<F, Fut> PricingEngine<F>
where
    F: Fn(MonitorEvent) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    pub fn new(
        client: Arc<ChainClient>,
        fork_url: &str,
        ws_url: &str,
        monitor_callback: F,
    ) -> Result<Self> {
        let simulator = ForkSimulator::new(fork_url)?;
        let monitor = MempoolMonitor::new(ws_url.to_string(), monitor_callback);

        Ok(Self {
            client,
            simulator,
            monitor,
            pools: HashMap::new(),
            router: None,
        })
    }

    pub async fn load_pools(&mut self, pool_addresses: Vec<Address>) -> Result<()> {
        for addr in pool_addresses {
            let pair = Pool::from_chain(addr, &self.client, None).await?;
            self.pools.insert(addr, pair);
        }

        let pools_list: Vec<Pool> = self.pools.values().cloned().collect();
        self.router = Some(RouteFinder::new(pools_list));
        Ok(())
    }

    pub async fn refresh_pool(&mut self, address: Address) -> Result<()> {
        if self.pools.contains_key(&address) {
            let new_pair = Pool::from_chain(address, &self.client, None).await?;
            self.pools.insert(address, new_pair);

            let pools_list: Vec<Pool> = self.pools.values().cloned().collect();
            self.router = Some(RouteFinder::new(pools_list));
        }
        Ok(())
    }

    pub async fn get_quote(
        &self,
        token_in: &Token,
        token_out: &Token,
        amount_in: U256,
        gas_price_gwei: u64,
        router_address: Address,
        sender: Address,
    ) -> Result<Quote> {
        let router = self
            .router
            .as_ref()
            .ok_or_else(|| anyhow!("Router not initialized"))?;

        let (route, net_output) =
            router.find_best_route(token_in, token_out, amount_in, gas_price_gwei, 3)?;

        let sim_result = self
            .simulator
            .simulate_route(router_address.0, &route, amount_in, sender.0)
            .await?;

        if !sim_result.success {
            return Err(anyhow!("Simulation failed: {:?}", sim_result.error));
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Quote {
            route,
            amount_in,
            expected_output: net_output,
            simulated_output: sim_result.amount_out,
            gas_estimate: sim_result.gas_used,
            timestamp,
        })
    }

    pub async fn scan_for_arbitrage(
        &self,
        token_in: &Token,
        amount_in: U256,
        router_address: Address,
        sender: Address,
    ) -> Vec<Quote> {
        let mut opportunities = Vec::new();

        if let Some(router) = &self.router {
            let loops = router.find_arbitrage_loops(token_in, amount_in, 3);

            for route in loops {
                if let Ok(sim_result) = self
                    .simulator
                    .simulate_route(router_address.0, &route, amount_in, sender.0)
                    .await
                {
                    if sim_result.success && sim_result.amount_out > amount_in {
                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        opportunities.push(Quote {
                            route,
                            amount_in,
                            expected_output: sim_result.amount_out,
                            simulated_output: sim_result.amount_out,
                            gas_estimate: sim_result.gas_used,
                            timestamp,
                        });
                    }
                }
            }
        }

        opportunities
    }

    pub async fn start_monitor(&self) -> Result<()> {
        self.monitor.start().await
    }
}
