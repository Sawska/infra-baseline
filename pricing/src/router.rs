#![allow(clippy::collapsible_if)]
#![allow(clippy::too_many_arguments)]
use crate::amm::{Pool, Token};
use alloy_primitives::U256;
use anyhow::{anyhow, Result};
use arb_core::Address;
use std::collections::{HashMap, HashSet};

/// Represents a swap route through one or more pools.
#[derive(Clone, Debug)]
pub struct Route {
    pub pools: Vec<Pool>,
    pub path: Vec<Token>,
}

impl Route {
    pub fn new(pools: Vec<Pool>, path: Vec<Token>) -> Self {
        Self { pools, path }
    }

    pub fn num_hops(&self) -> usize {
        self.pools.len()
    }

    /// Simulate full route, return final output.
    pub fn get_output(&self, amount_in: U256) -> Result<U256> {
        let mut current_amount = amount_in;
        for (i, pool) in self.pools.iter().enumerate() {
            let token_in = &self.path[i];
            current_amount = pool.get_amount_out(current_amount, token_in)?;
        }
        Ok(current_amount)
    }

    /// Return amount at each step: [input, after_hop1, after_hop2, ...]
    pub fn get_intermediate_amounts(&self, amount_in: U256) -> Result<Vec<U256>> {
        let mut amounts = vec![amount_in];
        let mut current_amount = amount_in;
        for (i, pool) in self.pools.iter().enumerate() {
            let token_in = &self.path[i];
            current_amount = pool.get_amount_out(current_amount, token_in)?;
            amounts.push(current_amount);
        }
        Ok(amounts)
    }

    /// Estimate gas: ~150k base + ~100k per hop.
    pub fn estimate_gas(&self) -> u64 {
        150_000 + (100_000 * self.pools.len() as u64)
    }
}

/// Detailed breakdown of a route's performance
#[derive(Debug, Clone)]
pub struct RouteAnalysis {
    pub route: Route,
    pub gross_output: U256,
    pub gas_estimate: u64,
    pub gas_cost_eth: U256,
    pub gas_cost_token: Option<U256>,
    pub net_output: U256,
}

/// Finds optimal routes between tokens.
pub struct RouteFinder {
    pub pools: Vec<Pool>,
    graph: HashMap<Address, Vec<(Pool, Token)>>,
}

impl RouteFinder {
    pub fn new(pools: Vec<Pool>) -> Self {
        let graph = Self::build_graph(&pools);
        Self { pools, graph }
    }

    /// Build adjacency graph: token -> [(pool, other_token), ...]
    fn build_graph(pools: &[Pool]) -> HashMap<Address, Vec<(Pool, Token)>> {
        let mut graph = HashMap::new();
        for pool in pools {
            let (t0, t1) = pool.tokens();

            graph
                .entry(t0.address)
                .or_insert_with(Vec::new)
                .push((pool.clone(), t1.clone()));

            graph
                .entry(t1.address)
                .or_insert_with(Vec::new)
                .push((pool.clone(), t0.clone()));
        }
        graph
    }

    /// Find all possible routes up to max_hops using DFS.
    pub fn find_all_routes(
        &self,
        token_in: &Token,
        token_out: &Token,
        max_hops: usize,
    ) -> Vec<Route> {
        let mut results = Vec::new();
        let mut current_path_tokens = vec![token_in.clone()];
        let mut current_path_pools = Vec::new();
        let mut visited = HashSet::new();

        // Start DFS
        visited.insert(token_in.address);

        self.dfs(
            token_in,
            token_out,
            max_hops,
            &mut visited,
            &mut current_path_tokens,
            &mut current_path_pools,
            &mut results,
        );

        results
    }

    fn dfs(
        &self,
        current_token: &Token,
        target_token: &Token,
        hops_remaining: usize,
        visited: &mut HashSet<Address>,
        path_tokens: &mut Vec<Token>,
        path_pools: &mut Vec<Pool>,
        results: &mut Vec<Route>,
    ) {
        if current_token.address == target_token.address {
            if !path_pools.is_empty() {
                results.push(Route::new(path_pools.clone(), path_tokens.clone()));
            }
            return;
        }

        if hops_remaining == 0 {
            return;
        }

        if let Some(neighbors) = self.graph.get(&current_token.address) {
            for (pool, next_token) in neighbors {
                if !visited.contains(&next_token.address) {
                    visited.insert(next_token.address);
                    path_tokens.push(next_token.clone());
                    path_pools.push(pool.clone());

                    self.dfs(
                        next_token,
                        target_token,
                        hops_remaining - 1,
                        visited,
                        path_tokens,
                        path_pools,
                        results,
                    );

                    path_pools.pop();
                    path_tokens.pop();
                    visited.remove(&next_token.address);
                }
            }
        }
    }

    /// Find route that maximizes NET output (after gas).
    /// Returns (best_route, net_output).
    pub fn find_best_route(
        &self,
        token_in: &Token,
        token_out: &Token,
        amount_in: U256,
        gas_price_gwei: u64,
        max_hops: usize,
    ) -> Result<(Route, U256)> {
        let analyses =
            self.compare_routes(token_in, token_out, amount_in, gas_price_gwei, max_hops)?;

        analyses
            .into_iter()
            .max_by_key(|a| a.net_output)
            .map(|a| (a.route, a.net_output))
            .ok_or_else(|| anyhow!("No route found"))
    }

    /// Compare all routes with detailed breakdown.
    pub fn compare_routes(
        &self,
        token_in: &Token,
        token_out: &Token,
        amount_in: U256,
        gas_price_gwei: u64,
        max_hops: usize,
    ) -> Result<Vec<RouteAnalysis>> {
        let routes = self.find_all_routes(token_in, token_out, max_hops);
        let mut results = Vec::new();

        for route in routes {
            let gross_output = route.get_output(amount_in).unwrap_or(U256::ZERO);
            let gas_estimate = route.estimate_gas();

            let gas_price_wei = U256::from(gas_price_gwei) * U256::from(1_000_000_000);
            let gas_cost_eth = U256::from(gas_estimate) * gas_price_wei;

            let gas_cost_token = self.convert_eth_to_token_cost(gas_cost_eth, token_out);

            let net_output = if let Some(cost) = gas_cost_token {
                gross_output.saturating_sub(cost)
            } else {
                gross_output
            };

            results.push(RouteAnalysis {
                route,
                gross_output,
                gas_estimate,
                gas_cost_eth,
                gas_cost_token,
                net_output,
            });
        }

        Ok(results)
    }

    /// Helper to estimate gas cost in terms of the output token.
    /// This is a simplified heuristic: it looks for a direct WETH pair in the known pools.
    fn convert_eth_to_token_cost(&self, gas_cost_eth: U256, token_out: &Token) -> Option<U256> {
        if token_out.symbol == "WETH" || token_out.symbol == "ETH" {
            return Some(gas_cost_eth);
        }

        if let Some(neighbors) = self.graph.get(&token_out.address) {
            for (pool, neighbor_token) in neighbors {
                if neighbor_token.symbol == "WETH" || neighbor_token.symbol == "ETH" {
                    return pool.get_amount_out(gas_cost_eth, neighbor_token).ok();
                }
            }
        }

        None
    }
    pub fn find_arbitrage_loops(
        &self,
        token_in: &Token,
        amount_in: U256,
        max_hops: usize,
    ) -> Vec<Route> {
        let mut arbitrage_ops = Vec::new();
        if let Some(neighbors) = self.graph.get(&token_in.address) {
            for (pool, next_token) in neighbors {
                if max_hops > 1 {
                    let sub_routes = self.find_all_routes(next_token, token_in, max_hops - 1);

                    for sub_route in sub_routes {
                        let mut new_pools = vec![pool.clone()];
                        new_pools.extend(sub_route.pools);

                        let mut new_path = vec![token_in.clone()];
                        new_path.extend(sub_route.path);

                        let full_route = Route::new(new_pools, new_path);

                        if let Ok(amount_out) = full_route.get_output(amount_in) {
                            if amount_out > amount_in {
                                arbitrage_ops.push(full_route);
                            }
                        }
                    }
                }
            }
        }

        arbitrage_ops
    }
}
