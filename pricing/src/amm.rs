use alloy_primitives::{Bytes, U256, map::HashMap};
use alloy_rpc_types::BlockNumberOrTag;
use alloy_sol_types::SolCall;
use anyhow::{Context, Result, anyhow};
use arb_chain::ChainClient;
use arb_core::{Address, TokenAmount, TransactionRequest};
use rust_decimal::prelude::*;
use std::str::FromStr;

/// Represents the metadata of a Token.
/// Note: arb_core::TokenAmount contains a specific amount, this struct describes the token itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub address: Address,
    pub decimals: u8,
    pub symbol: String,
}

impl Token {
    pub fn new(address: Address, decimals: u8, symbol: String) -> Self {
        Self {
            address,
            decimals,
            symbol,
        }
    }
}

/// Represents a Uniswap V2 liquidity pair.
/// All math uses integers (U256) only for core swap logic.
#[derive(Clone, Debug)]
pub struct UniswapV2Pair {
    pub address: Address,
    pub token0: Token,
    pub token1: Token,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,
}

impl UniswapV2Pair {
    /// Creates a new Uniswap V2 Pair instance.
    pub fn new(
        address: Address,
        token0: Token,
        token1: Token,
        reserve0: U256,
        reserve1: U256,
        fee_bps: u32,
    ) -> Self {
        Self {
            address,
            token0,
            token1,
            reserve0,
            reserve1,
            fee_bps,
        }
    }

    /// Calculate output amount for a given input.
    /// Matches Solidity exactly:
    /// amount_in_with_fee = amount_in * (10000 - fee_bps)
    /// numerator = amount_in_with_fee * reserve_out
    /// denominator = reserve_in * 10000 + amount_in_with_fee
    /// amount_out = numerator // denominator
    pub fn get_amount_out(&self, amount_in: U256, token_in: &Token) -> Result<U256> {
        let (reserve_in, reserve_out) = self.get_reserves_ordered(token_in)?;

        if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return Ok(U256::ZERO);
        }

        let fee_multiplier = U256::from(10000 - self.fee_bps);
        let amount_in_with_fee = amount_in
            .checked_mul(fee_multiplier)
            .ok_or_else(|| anyhow!("Overflow calculating amount_in_with_fee"))?;

        let numerator = amount_in_with_fee
            .checked_mul(reserve_out)
            .ok_or_else(|| anyhow!("Overflow calculating numerator"))?;

        let denominator_base = reserve_in
            .checked_mul(U256::from(10000))
            .ok_or_else(|| anyhow!("Overflow calculating denominator base"))?;

        let denominator = denominator_base
            .checked_add(amount_in_with_fee)
            .ok_or_else(|| anyhow!("Overflow calculating denominator"))?;

        if denominator.is_zero() {
            return Err(anyhow!("Denominator is zero"));
        }

        Ok(numerator / denominator)
    }

    /// Calculate required input for desired output.
    /// (Inverse of get_amount_out)
    /// numerator = reserve_in * amount_out * 10000
    /// denominator = (reserve_out - amount_out) * (10000 - fee_bps)
    /// amount_in = (numerator / denominator) + 1
    pub fn get_amount_in(&self, amount_out: U256, token_out: &Token) -> Result<U256> {
        let (reserve_in, reserve_out) = self.get_reserves_ordered(token_out)?;

        if amount_out.is_zero() || amount_out >= reserve_out {
            return Err(anyhow!("Insufficient liquidity"));
        }

        let numerator = reserve_in
            .checked_mul(amount_out)
            .and_then(|x| x.checked_mul(U256::from(10000)))
            .ok_or_else(|| anyhow!("Overflow in numerator"))?;

        let fee_multiplier = U256::from(10000 - self.fee_bps);
        let denominator = reserve_out
            .checked_sub(amount_out)
            .and_then(|x| x.checked_mul(fee_multiplier))
            .ok_or_else(|| anyhow!("Overflow or underflow in denominator"))?;

        if denominator.is_zero() {
            return Err(anyhow!("Denominator is zero"));
        }

        Ok((numerator / denominator).saturating_add(U256::from(1)))
    }

    /// Returns spot price (for display only, not calculations).
    /// Price = (reserve_out / 10^dec_out) / (reserve_in / 10^dec_in)
    pub fn get_spot_price(&self, token_in: &Token) -> Result<Decimal> {
        let (reserve_in, reserve_out) = self.get_reserves_ordered(token_in)?;

        let r_in_d = self.u256_to_decimal(reserve_in, token_in.decimals);

        let token_out = if token_in.address == self.token0.address {
            &self.token1
        } else {
            &self.token0
        };
        let r_out_d = self.u256_to_decimal(reserve_out, token_out.decimals);

        if r_in_d.is_zero() {
            return Ok(Decimal::ZERO);
        }

        Ok(r_out_d / r_in_d)
    }

    /// Returns actual execution price for given trade size.
    /// Exec Price = amount_out / amount_in
    pub fn get_execution_price(&self, amount_in: U256, token_in: &Token) -> Result<Decimal> {
        let amount_out = self.get_amount_out(amount_in, token_in)?;

        let a_in_d = self.u256_to_decimal(amount_in, token_in.decimals);

        let token_out = if token_in.address == self.token0.address {
            &self.token1
        } else {
            &self.token0
        };
        let a_out_d = self.u256_to_decimal(amount_out, token_out.decimals);

        if a_in_d.is_zero() {
            return Ok(Decimal::ZERO);
        }

        Ok(a_out_d / a_in_d)
    }

    /// Returns price impact as a decimal (0.01 = 1%).
    /// Impact = (Spot Price - Execution Price) / Spot Price
    pub fn get_price_impact(&self, amount_in: U256, token_in: &Token) -> Result<Decimal> {
        let spot = self.get_spot_price(token_in)?;
        let exec = self.get_execution_price(amount_in, token_in)?;

        if spot.is_zero() {
            return Ok(Decimal::ZERO);
        }

        Ok((spot - exec) / spot)
    }

    /// Returns a NEW pair with updated reserves after the swap.
    /// (Immutable update)
    pub fn simulate_swap(&self, amount_in: U256, token_in: &Token) -> Result<Self> {
        let amount_out = self.get_amount_out(amount_in, token_in)?;

        let mut new_pair = self.clone();

        if token_in.address == self.token0.address {
            new_pair.reserve0 = self
                .reserve0
                .checked_add(amount_in)
                .ok_or(anyhow!("Overflow adding reserve0"))?;
            new_pair.reserve1 = self
                .reserve1
                .checked_sub(amount_out)
                .ok_or(anyhow!("Underflow subtracting reserve1"))?;
        } else if token_in.address == self.token1.address {
            new_pair.reserve1 = self
                .reserve1
                .checked_add(amount_in)
                .ok_or(anyhow!("Overflow adding reserve1"))?;
            new_pair.reserve0 = self
                .reserve0
                .checked_sub(amount_out)
                .ok_or(anyhow!("Underflow subtracting reserve0"))?;
        } else {
            return Err(anyhow!("Token not in pair"));
        }

        Ok(new_pair)
    }

    /// Fetch pair data from on-chain.
    pub async fn from_chain(
        address: Address,
        client: &ChainClient,
        block: Option<u64>,
    ) -> Result<Self> {
        alloy_sol_types::sol! {
            function token0() external view returns (address);
            function token1() external view returns (address);
            function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
            function decimals() external view returns (uint8);
            function symbol() external view returns (string);
        }

        let block_tag = block.map(BlockNumberOrTag::Number);

        let t0_req = Self::make_req(address, token0Call {}.abi_encode());
        let t0_res = client
            .call(&t0_req, block_tag)
            .await
            .context("Failed to fetch token0")?;
        let t0_raw = token0Call::abi_decode_returns(&t0_res)?;
        let t0_addr = Address(t0_raw);

        let t1_req = Self::make_req(address, token1Call {}.abi_encode());
        let t1_res = client
            .call(&t1_req, block_tag)
            .await
            .context("Failed to fetch token1")?;
        let t1_raw = token1Call::abi_decode_returns(&t1_res)?;
        let t1_addr = Address(t1_raw);

        let token0 = Self::fetch_token_details(t0_addr, client, block_tag).await?;
        let token1 = Self::fetch_token_details(t1_addr, client, block_tag).await?;

        let reserves_req = Self::make_req(address, getReservesCall {}.abi_encode());
        let res_bytes = client
            .call(&reserves_req, block_tag)
            .await
            .context("Failed to fetch reserves")?;

        let reserves = getReservesCall::abi_decode_returns(&res_bytes)?;
        let r0_raw = reserves.reserve0;
        let r1_raw = reserves.reserve1;

        Ok(Self::new(
            address,
            token0,
            token1,
            U256::from(r0_raw),
            U256::from(r1_raw),
            30,
        ))
    }

    fn make_req(to: Address, data: Vec<u8>) -> TransactionRequest {
        TransactionRequest {
            to,
            from: None,
            value: TokenAmount {
                raw: U256::ZERO,
                decimals: 18,
                symbol: None,
            },
            data: Bytes::from(data),
            nonce: None,
            gas_limit: None,
            max_fee_per_gas: None,
            max_priority_fee: None,
            chain_id: 1,
        }
    }

    async fn fetch_token_details(
        addr: Address,
        client: &ChainClient,
        block: Option<BlockNumberOrTag>,
    ) -> Result<Token> {
        alloy_sol_types::sol! {
            function decimals() external view returns (uint8);
            function symbol() external view returns (string);
        }

        let d_req = Self::make_req(addr, decimalsCall {}.abi_encode());
        let d_res = client.call(&d_req, block).await.unwrap_or_default();
        let decimals = decimalsCall::abi_decode_returns(&d_res).unwrap_or(18);

        let s_req = Self::make_req(addr, symbolCall {}.abi_encode());
        let s_res = client.call(&s_req, block).await.unwrap_or_default();
        let symbol = symbolCall::abi_decode_returns(&s_res).unwrap_or_else(|_| "UNK".to_string());

        Ok(Token::new(addr, decimals, symbol))
    }

    /// Helper to get reserves ordered by (reserve_in, reserve_out)
    pub fn get_reserves_ordered(&self, token_in: &Token) -> Result<(U256, U256)> {
        if token_in.address == self.token0.address {
            Ok((self.reserve0, self.reserve1))
        } else if token_in.address == self.token1.address {
            Ok((self.reserve1, self.reserve0))
        } else {
            Err(anyhow!("Token not in pair"))
        }
    }

    /// Helper to convert U256 to Decimal using token decimals
    fn u256_to_decimal(&self, val: U256, decimals: u8) -> Decimal {
        let s = val.to_string();
        let d = Decimal::from_str(&s).unwrap_or(Decimal::ZERO);
        let multiplier = Decimal::from(10u64.pow(decimals as u32));
        d / multiplier
    }
}

/// Simplified Tick Data structure for V3
#[derive(Clone, Debug, Default)]
pub struct TickData {
    pub liquidity_net: i128,
    pub liquidity_gross: u128,
}

/// Represents a Uniswap V3 concentrated liquidity pool.
/// Handles simplified single-tick math for pricing.
#[derive(Clone, Debug)]
pub struct UniswapV3Pool {
    pub address: Address,
    pub token0: Token,
    pub token1: Token,
    pub liquidity: u128,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub fee: u32,
    pub tick_spacing: i32,
    pub ticks: HashMap<i32, TickData>,
}

impl UniswapV3Pool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address: Address,
        token0: Token,
        token1: Token,
        liquidity: u128,
        sqrt_price_x96: U256,
        tick: i32,
        fee: u32,
        tick_spacing: i32,
    ) -> Self {
        Self {
            address,
            token0,
            token1,
            liquidity,
            sqrt_price_x96,
            tick,
            fee,
            tick_spacing,
            ticks: HashMap::default(),
        }
    }

    /// Calculate output amount assuming trade stays within current tick.
    /// This is an approximation for small trades.
    pub fn get_amount_out(&self, amount_in: U256, token_in: &Token) -> Result<U256> {
        if amount_in.is_zero() {
            return Ok(U256::ZERO);
        }

        let zero_for_one = token_in.address == self.token0.address;

        let mut state = {
            let amount_specified = amount_in.to::<i128>();
            SwapState {
                amount_specified_remaining: amount_specified,
                amount_calculated: 0,
                sqrt_price_x96: self.sqrt_price_x96,
                tick: self.tick,
                liquidity: self.liquidity,
            }
        };

        while state.amount_specified_remaining > 0 {
            let mut step = StepComputations {
                _sqrt_price_start_x96: state.sqrt_price_x96,
                ..Default::default()
            };

            let (next_tick, initialized) = self.get_next_tick(state.tick, zero_for_one);
            step.tick_next = next_tick;

            step.sqrt_price_next_x96 = self.get_sqrt_ratio_at_tick(step.tick_next)?;

            let (sqrt_price_next_x96, amount_in_step, amount_out_step, fee_amount) = self
                .compute_swap_step(
                    state.sqrt_price_x96,
                    step.sqrt_price_next_x96,
                    state.liquidity,
                    state.amount_specified_remaining,
                    self.fee,
                )?;

            state.sqrt_price_x96 = sqrt_price_next_x96;
            state.amount_specified_remaining -= (amount_in_step + fee_amount) as i128;
            state.amount_calculated += amount_out_step as i128;

            if state.sqrt_price_x96 == step.sqrt_price_next_x96 && initialized {
                let tick_data = self.ticks.get(&step.tick_next).cloned().unwrap_or_default();

                if zero_for_one {
                    state.liquidity = if tick_data.liquidity_net < 0 {
                        state
                            .liquidity
                            .saturating_sub(tick_data.liquidity_net.unsigned_abs())
                    } else {
                        state
                            .liquidity
                            .checked_add(tick_data.liquidity_net as u128)
                            .unwrap_or(state.liquidity)
                    };
                    state.tick = step.tick_next - 1;
                } else {
                    state.liquidity = if tick_data.liquidity_net < 0 {
                        state
                            .liquidity
                            .saturating_sub(tick_data.liquidity_net.unsigned_abs())
                    } else {
                        state
                            .liquidity
                            .checked_add(tick_data.liquidity_net as u128)
                            .unwrap_or(state.liquidity)
                    };
                    state.tick = step.tick_next;
                }
            } else {
                state.tick = self.get_tick_at_sqrt_ratio(state.sqrt_price_x96)?;
            }
        }

        Ok(U256::from(state.amount_calculated))
    }

    pub fn get_spot_price(&self, token_in: &Token) -> Result<Decimal> {
        let sqrt_price_dec = self.u256_to_decimal(self.sqrt_price_x96);
        let q96_dec = self.u256_to_decimal(U256::from(1) << 96);

        let p = (sqrt_price_dec / q96_dec).powi(2);

        if token_in.address == self.token0.address {
            let adj =
                Decimal::from(10).powi(self.token0.decimals as i64 - self.token1.decimals as i64);
            Ok(p * adj)
        } else {
            if p.is_zero() {
                return Ok(Decimal::ZERO);
            }
            let inv_p = Decimal::ONE / p;
            let adj =
                Decimal::from(10).powi(self.token1.decimals as i64 - self.token0.decimals as i64);
            Ok(inv_p * adj)
        }
    }

    pub fn get_execution_price(&self, amount_in: U256, token_in: &Token) -> Result<Decimal> {
        let amount_out = self.get_amount_out(amount_in, token_in)?;

        let a_in_d = self.u256_val_to_decimal(amount_in, token_in.decimals);

        let token_out = if token_in.address == self.token0.address {
            &self.token1
        } else {
            &self.token0
        };
        let a_out_d = self.u256_val_to_decimal(amount_out, token_out.decimals);

        if a_in_d.is_zero() {
            return Ok(Decimal::ZERO);
        }

        Ok(a_out_d / a_in_d)
    }

    pub fn get_price_impact(&self, amount_in: U256, token_in: &Token) -> Result<Decimal> {
        let spot = self.get_spot_price(token_in)?;
        let exec = self.get_execution_price(amount_in, token_in)?;

        if spot.is_zero() {
            return Ok(Decimal::ZERO);
        }

        Ok((spot - exec) / spot)
    }

    fn u256_to_decimal(&self, val: U256) -> Decimal {
        Decimal::from_str(&val.to_string()).unwrap_or(Decimal::ZERO)
    }

    fn u256_val_to_decimal(&self, val: U256, decimals: u8) -> Decimal {
        let s = val.to_string();
        let d = Decimal::from_str(&s).unwrap_or(Decimal::ZERO);
        let multiplier = Decimal::from(10u64.pow(decimals as u32));
        d / multiplier
    }

    fn get_next_tick(&self, current_tick: i32, zero_for_one: bool) -> (i32, bool) {
        let mut initialized_ticks: Vec<i32> = self.ticks.keys().cloned().collect();
        initialized_ticks.sort();

        if zero_for_one {
            let found = initialized_ticks.iter().rev().find(|&&t| t <= current_tick);
            match found {
                Some(&t) => (t, true),
                None => (-887272, false),
            }
        } else {
            let found = initialized_ticks.iter().find(|&&t| t > current_tick);
            match found {
                Some(&t) => (t, true),
                None => (887272, false),
            }
        }
    }

    /// Simplified V3 step math.
    fn compute_swap_step(
        &self,
        sqrt_p_start: U256,
        sqrt_p_target: U256,
        liquidity: u128,
        amount_remaining: i128,
        fee_pips: u32,
    ) -> Result<(U256, u128, u128, u128)> {
        let zero_for_one = sqrt_p_start >= sqrt_p_target;
        let amount_remaining_less_fee =
            (amount_remaining as u128 * (1_000_000 - fee_pips as u128)) / 1_000_000;

        let liquidity_u256 = U256::from(liquidity);
        let q96 = U256::from(1) << 96;

        let amount_in_max = if zero_for_one {
            let num = liquidity_u256
                .checked_mul(
                    sqrt_p_start
                        .checked_sub(sqrt_p_target)
                        .unwrap_or(U256::ZERO),
                )
                .unwrap_or(U256::ZERO)
                .checked_mul(q96)
                .unwrap_or(U256::ZERO);
            let den = sqrt_p_start
                .checked_mul(sqrt_p_target)
                .unwrap_or(U256::from(1));
            num / den
        } else {
            liquidity_u256
                .checked_mul(
                    sqrt_p_target
                        .checked_sub(sqrt_p_start)
                        .unwrap_or(U256::ZERO),
                )
                .unwrap_or(U256::ZERO)
                / q96
        };

        if amount_remaining_less_fee >= amount_in_max.to::<u128>() {
            let amount_out = if zero_for_one {
                liquidity_u256
                    .checked_mul(
                        sqrt_p_start
                            .checked_sub(sqrt_p_target)
                            .unwrap_or(U256::ZERO),
                    )
                    .unwrap_or(U256::ZERO)
                    / q96
            } else {
                let num = liquidity_u256
                    .checked_mul(
                        sqrt_p_target
                            .checked_sub(sqrt_p_start)
                            .unwrap_or(U256::ZERO),
                    )
                    .unwrap_or(U256::ZERO)
                    .checked_mul(q96)
                    .unwrap_or(U256::ZERO);
                let den = sqrt_p_start
                    .checked_mul(sqrt_p_target)
                    .unwrap_or(U256::from(1));
                num / den
            };
            let fee = (amount_in_max.to::<u128>() * fee_pips as u128).div_ceil(1_000_000);
            Ok((
                sqrt_p_target,
                amount_in_max.to::<u128>(),
                amount_out.to::<u128>(),
                fee,
            ))
        } else {
            let sqrt_p_next = if zero_for_one {
                let num = liquidity_u256
                    .checked_mul(sqrt_p_start)
                    .unwrap()
                    .checked_mul(q96)
                    .unwrap();
                let den = liquidity_u256
                    .checked_mul(q96)
                    .unwrap()
                    .checked_add(
                        U256::from(amount_remaining_less_fee)
                            .checked_mul(sqrt_p_start)
                            .unwrap(),
                    )
                    .unwrap();
                num / den
            } else {
                sqrt_p_start
                    + (U256::from(amount_remaining_less_fee)
                        .checked_mul(q96)
                        .unwrap()
                        / liquidity_u256)
            };

            let amount_out = if zero_for_one {
                liquidity_u256
                    .checked_mul(sqrt_p_start.checked_sub(sqrt_p_next).unwrap())
                    .unwrap()
                    / q96
            } else {
                let num = liquidity_u256
                    .checked_mul(sqrt_p_next.checked_sub(sqrt_p_start).unwrap())
                    .unwrap()
                    .checked_mul(q96)
                    .unwrap();
                let den = sqrt_p_start.checked_mul(sqrt_p_next).unwrap();
                num / den
            };

            Ok((
                sqrt_p_next,
                amount_remaining_less_fee,
                amount_out.to::<u128>(),
                (amount_remaining as u128 - amount_remaining_less_fee),
            ))
        }
    }

    /// TickMath: get sqrt ratio at tick.
    fn get_sqrt_ratio_at_tick(&self, tick: i32) -> Result<U256> {
        let ratio = 1.0001f64.powi(tick).sqrt();
        let q96 = 2.0f64.powi(96);
        let val = (ratio * q96) as u128;
        Ok(U256::from(val))
    }

    fn get_tick_at_sqrt_ratio(&self, sqrt_p: U256) -> Result<i32> {
        let q96 = 2.0f64.powi(96);
        let ratio = (sqrt_p.to::<u128>() as f64) / q96;
        let tick = (ratio.powi(2).ln() / 1.0001f64.ln()).round() as i32;
        Ok(tick)
    }

    /// Fetch V3 pool data from on-chain.
    pub async fn from_chain(
        address: Address,
        client: &ChainClient,
        block: Option<u64>,
    ) -> Result<Self> {
        alloy_sol_types::sol! {
            function token0() external view returns (address);
            function token1() external view returns (address);
            function liquidity() external view returns (uint128);
            function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);
            function fee() external view returns (uint24);
            function tickSpacing() external view returns (int24);
        }
        let b = block.map(BlockNumberOrTag::Number);
        let t0 = Address(token0Call::abi_decode_returns(
            &client
                .call(
                    &UniswapV2Pair::make_req(address, token0Call {}.abi_encode()),
                    b,
                )
                .await?,
        )?);
        let t1 = Address(token1Call::abi_decode_returns(
            &client
                .call(
                    &UniswapV2Pair::make_req(address, token1Call {}.abi_encode()),
                    b,
                )
                .await?,
        )?);
        let token0 = UniswapV2Pair::fetch_token_details(t0, client, b).await?;
        let token1 = UniswapV2Pair::fetch_token_details(t1, client, b).await?;

        let liq = liquidityCall::abi_decode_returns(
            &client
                .call(
                    &UniswapV2Pair::make_req(address, liquidityCall {}.abi_encode()),
                    b,
                )
                .await?,
        )?;
        let s0 = slot0Call::abi_decode_returns(
            &client
                .call(
                    &UniswapV2Pair::make_req(address, slot0Call {}.abi_encode()),
                    b,
                )
                .await?,
        )?;
        let fee = feeCall::abi_decode_returns(
            &client
                .call(
                    &UniswapV2Pair::make_req(address, feeCall {}.abi_encode()),
                    b,
                )
                .await?,
        )?;
        let ts = tickSpacingCall::abi_decode_returns(
            &client
                .call(
                    &UniswapV2Pair::make_req(address, tickSpacingCall {}.abi_encode()),
                    b,
                )
                .await?,
        )?;

        let pool = Self::new(
            address,
            token0,
            token1,
            liq,
            U256::from(s0.sqrtPriceX96),
            s0.tick.as_i32(),
            fee.to::<u32>(),
            ts.as_i32(),
        );

        Ok(pool)
    }
}

struct SwapState {
    amount_specified_remaining: i128,
    amount_calculated: i128,
    sqrt_price_x96: U256,
    tick: i32,
    liquidity: u128,
}

#[derive(Default)]
struct StepComputations {
    _sqrt_price_start_x96: U256,
    tick_next: i32,
    sqrt_price_next_x96: U256,
}

#[derive(Clone, Debug)]
pub enum Pool {
    V2(UniswapV2Pair),
    V3(UniswapV3Pool),
}

impl Pool {
    pub fn address(&self) -> Address {
        match self {
            Pool::V2(p) => p.address,
            Pool::V3(p) => p.address,
        }
    }

    pub fn tokens(&self) -> (Token, Token) {
        match self {
            Pool::V2(p) => (p.token0.clone(), p.token1.clone()),
            Pool::V3(p) => (p.token0.clone(), p.token1.clone()),
        }
    }

    pub fn get_amount_out(&self, amount_in: U256, token_in: &Token) -> Result<U256> {
        match self {
            Pool::V2(p) => p.get_amount_out(amount_in, token_in),
            Pool::V3(p) => p.get_amount_out(amount_in, token_in),
        }
    }

    pub fn get_spot_price(&self, token_in: &Token) -> Result<Decimal> {
        match self {
            Pool::V2(p) => p.get_spot_price(token_in),
            Pool::V3(p) => p.get_spot_price(token_in),
        }
    }

    pub fn get_execution_price(&self, amount_in: U256, token_in: &Token) -> Result<Decimal> {
        match self {
            Pool::V2(p) => p.get_execution_price(amount_in, token_in),
            Pool::V3(p) => p.get_execution_price(amount_in, token_in),
        }
    }

    pub fn get_price_impact(&self, amount_in: U256, token_in: &Token) -> Result<Decimal> {
        match self {
            Pool::V2(p) => p.get_price_impact(amount_in, token_in),
            Pool::V3(p) => p.get_price_impact(amount_in, token_in),
        }
    }

    pub async fn from_chain(
        address: Address,
        client: &ChainClient,
        block: Option<u64>,
    ) -> Result<Self> {
        if let Ok(v2_pair) = UniswapV2Pair::from_chain(address, client, block).await {
            return Ok(Pool::V2(v2_pair));
        }

        match UniswapV3Pool::from_chain(address, client, block).await {
            Ok(v3_pool) => Ok(Pool::V3(v3_pool)),
            Err(_) => Err(anyhow::anyhow!(
                "Address {:?} is not a valid V2 or V3 pool",
                address
            )),
        }
    }
}
