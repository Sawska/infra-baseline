use crate::amm::{Pool, Token};
use crate::router::Route;
use alloy_network::Ethereum;
use alloy_primitives::{Address, U160, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use alloy_sol_types::SolCall;
use anyhow::{Result, anyhow};
use std::sync::Arc;
use url::Url;

alloy_sol_types::sol! {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] memory path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);


    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function getAmountOut(uint amountIn, uint reserveIn, uint reserveOut) external pure returns (uint amountOut);
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
    pub fn new(fork_url: &str) -> Result<Self> {
        let url = Url::parse(fork_url)?;
        let provider = ProviderBuilder::new().connect_http(url);
        Ok(Self {
            provider: Arc::new(provider),
        })
    }

    pub async fn simulate_pool_swap(
        &self,
        pool: &Pool,
        router_address: Address,
        amount_in: U256,
        token_in: &Token,
        sender: Address,
    ) -> Result<SimulationResult> {
        let (token0, token1) = pool.tokens();
        let token_out = if token_in.address == token0.address {
            token1
        } else {
            token0
        };

        let calldata = match pool {
            Pool::V2(_) => {
                let path = vec![token_in.address.0, token_out.address.0];
                swapExactTokensForTokensCall {
                    amountIn: amount_in,
                    amountOutMin: U256::ZERO,
                    path,
                    to: sender,
                    deadline: U256::MAX,
                }
                .abi_encode()
            }
            Pool::V3(v3) => {
                let params = ExactInputSingleParams {
                    tokenIn: token_in.address.0,
                    tokenOut: token_out.address.0,
                    fee: alloy_primitives::Uint::from(v3.fee),
                    recipient: sender,
                    deadline: U256::MAX,
                    amountIn: amount_in,
                    amountOutMinimum: U256::ZERO,
                    sqrtPriceLimitX96: U160::ZERO,
                };
                exactInputSingleCall { params }.abi_encode()
            }
        };

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
                    error: Some(format!("Gas estimation failed: {}", e)),
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
                    error: Some(format!("Execution failed: {}", e)),
                });
            }
        };
        match pool {
            Pool::V2(_) => match swapExactTokensForTokensCall::abi_decode_returns(&result_bytes) {
                Ok(amounts) => Ok(SimulationResult {
                    success: true,
                    amount_out: *amounts.last().unwrap_or(&U256::ZERO),
                    gas_used,
                    error: None,
                }),
                Err(e) => Ok(SimulationResult {
                    success: false,
                    amount_out: U256::ZERO,
                    gas_used,
                    error: Some(format!("V2 decode error: {}", e)),
                }),
            },
            Pool::V3(_) => match exactInputSingleCall::abi_decode_returns(&result_bytes) {
                Ok(amount_out) => Ok(SimulationResult {
                    success: true,
                    amount_out,
                    gas_used,
                    error: None,
                }),
                Err(e) => Ok(SimulationResult {
                    success: false,
                    amount_out: U256::ZERO,
                    gas_used,
                    error: Some(format!("V3 decode error: {}", e)),
                }),
            },
        }
    }

    pub async fn simulate_route(
        &self,
        router_address: Address,
        route: &Route,
        amount_in: U256,
        sender: Address,
    ) -> Result<SimulationResult> {
        let path: Vec<Address> = route.path.iter().map(|t| t.address.0).collect();
        self.simulate_v2_swap(router_address, amount_in, path, sender)
            .await
    }
    async fn simulate_v2_swap(
        &self,
        router_address: Address,
        amount_in: U256,
        path: Vec<Address>,
        sender: Address,
    ) -> Result<SimulationResult> {
        let call = swapExactTokensForTokensCall {
            amountIn: amount_in,
            amountOutMin: U256::ZERO,
            path,
            to: sender,
            deadline: U256::MAX,
        };

        let tx = TransactionRequest {
            from: Some(sender),
            to: Some(alloy_primitives::TxKind::Call(router_address)),
            input: TransactionInput::new(call.abi_encode().into()),
            ..Default::default()
        };

        let gas_used = self.provider.estimate_gas(tx.clone()).await.unwrap_or(0);
        let result_bytes = self.provider.call(tx).await?;

        match swapExactTokensForTokensCall::abi_decode_returns(&result_bytes) {
            Ok(res) => Ok(SimulationResult {
                success: true,
                amount_out: *res.last().unwrap_or(&U256::ZERO),
                gas_used,
                error: None,
            }),
            Err(e) => Ok(SimulationResult {
                success: false,
                amount_out: U256::ZERO,
                gas_used,
                error: Some(e.to_string()),
            }),
        }
    }

    pub async fn compare_simulation_vs_calculation(
        &self,
        pool: &Pool,
        router_address: Address,
        amount_in: U256,
        token_in: &Token,
        sender: Address,
    ) -> Result<ComparisonResult> {
        let calculated = pool.get_amount_out(amount_in, token_in)?;

        let sim_result = self
            .simulate_pool_swap(pool, router_address, amount_in, token_in, sender)
            .await?;

        if !sim_result.success {
            return Err(anyhow!(
                "Simulation failed during comparison: {:?}",
                sim_result.error
            ));
        }

        let simulated = sim_result.amount_out;
        let diff = if calculated > simulated {
            calculated - simulated
        } else {
            simulated - calculated
        };

        Ok(ComparisonResult {
            calculated,
            simulated,
            difference: diff,
            matches: calculated == simulated,
        })
    }

    pub async fn get_v2_reserves(&self, pair_address: Address) -> Result<(U256, U256)> {
        let call = getReservesCall {};
        let tx = TransactionRequest {
            to: Some(alloy_primitives::TxKind::Call(pair_address)),
            input: TransactionInput::new(call.abi_encode().into()),
            ..Default::default()
        };

        let result_bytes = self.provider.call(tx).await?;
        let decoded = getReservesCall::abi_decode_returns(&result_bytes)?;

        Ok((U256::from(decoded.reserve0), U256::from(decoded.reserve1)))
    }

    pub async fn call_uniswap_v2_get_amount_out(
        &self,
        router_address: Address,
        amount_in: U256,
        reserve_in: U256,
        reserve_out: U256,
    ) -> Result<U256> {
        let call = getAmountOutCall {
            amountIn: amount_in,
            reserveIn: reserve_in,
            reserveOut: reserve_out,
        };
        let tx = TransactionRequest {
            to: Some(alloy_primitives::TxKind::Call(router_address)),
            input: TransactionInput::new(call.abi_encode().into()),
            ..Default::default()
        };

        let result_bytes = self.provider.call(tx).await?;
        let decoded = getAmountOutCall::abi_decode_returns(&result_bytes)?;

        Ok(decoded)
    }
}
