use std::sync::Arc;

use crate::amm::UniswapV2Pair;
use crate::router::Route;
use alloy_network::Ethereum;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use alloy_sol_types::SolCall;
use anyhow::{Result, anyhow};
use url::Url;

alloy_sol_types::sol! {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] memory path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub success: bool,
    pub amount_out: U256,
    pub gas_used: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComparisonResult {
    pub calculated: U256,
    pub simulated: U256,
    pub difference: U256,
    pub matches: bool,
}

pub struct ForkSimulator {
    provider: Arc<dyn Provider<Ethereum>>,
}

impl ForkSimulator {
    /// fork_url: Local Anvil/Hardhat fork RPC
    pub fn new(fork_url: &str) -> Result<Self> {
        let url = Url::parse(fork_url)?;
        let provider = ProviderBuilder::new().connect_http(url);
        Ok(Self {
            provider: Arc::new(provider),
        })
    }

    /// Simulate a swap and return detailed results.
    /// Uses eth_call to get output and eth_estimateGas for gas usage.
    pub async fn simulate_swap(
        &self,
        router_address: Address,
        amount_in: U256,
        path: Vec<Address>,
        sender: Address,
    ) -> Result<SimulationResult> {
        let deadline = U256::MAX;
        let amount_out_min = U256::ZERO;

        let call = swapExactTokensForTokensCall {
            amountIn: amount_in,
            amountOutMin: amount_out_min,
            path: path.clone(),
            to: sender,
            deadline,
        };

        let calldata = call.abi_encode();

        let tx = TransactionRequest {
            from: Some(sender),
            to: Some(alloy_primitives::TxKind::Call(router_address)),
            input: TransactionInput::new(calldata.into()),
            ..Default::default()
        };

        let gas_used = match self.provider.estimate_gas(tx.clone()).await {
            Ok(gas) => gas,
            Err(e) => {
                return Ok(SimulationResult {
                    success: false,
                    amount_out: U256::ZERO,
                    gas_used: 0,
                    error: Some(e.to_string()),
                });
            }
        };

        let result_bytes = match self.provider.call(tx.clone()).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(SimulationResult {
                    success: false,
                    amount_out: U256::ZERO,
                    gas_used,
                    error: Some(e.to_string()),
                });
            }
        };

        match swapExactTokensForTokensCall::abi_decode_returns(&result_bytes) {
            Ok(return_val) => {
                let amount_out = return_val.last().cloned().unwrap_or(U256::ZERO);
                Ok(SimulationResult {
                    success: true,
                    amount_out,
                    gas_used,
                    error: None,
                })
            }
            Err(e) => Ok(SimulationResult {
                success: false,
                amount_out: U256::ZERO,
                gas_used,
                error: Some(format!("Decoding error: {}", e)),
            }),
        }
    }

    /// Simulate a multi-hop route.
    pub async fn simulate_route(
        &self,
        router_address: Address,
        route: &Route,
        amount_in: U256,
        sender: Address,
    ) -> Result<SimulationResult> {
        let path: Vec<Address> = route.path.iter().map(|t| t.address.0).collect();
        self.simulate_swap(router_address, amount_in, path, sender)
            .await
    }

    /// Compare our AMM math vs actual fork simulation.
    /// Useful for validation.
    pub async fn compare_simulation_vs_calculation(
        &self,
        pair: &UniswapV2Pair,
        router_address: Address,
        amount_in: U256,
        sender: Address,
    ) -> Result<ComparisonResult> {
        let token_in = &pair.token0;
        let token_out = &pair.token1;

        let calculated = pair.get_amount_out(amount_in, token_in)?;

        let path = vec![token_in.address.0, token_out.address.0];
        let sim_result = self
            .simulate_swap(router_address, amount_in, path, sender)
            .await?;

        if !sim_result.success {
            return Err(anyhow!(
                "Simulation failed during comparison: {:?}",
                sim_result.error
            ));
        }

        let diff = if calculated > sim_result.amount_out {
            calculated - sim_result.amount_out
        } else {
            sim_result.amount_out - calculated
        };

        Ok(ComparisonResult {
            calculated,
            simulated: sim_result.amount_out,
            difference: diff,
            matches: calculated == sim_result.amount_out,
        })
    }
}
