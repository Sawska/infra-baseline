pub mod gas_units {
    pub const ERC20_TRANSFER: u64 = 65_000;
    pub const UNISWAP_V2_SWAP: u64 = 130_000;
    pub const UNISWAP_V3_SWAP: u64 = 160_000;
    pub const TWO_HOP_V3: u64 = 280_000;
    pub const FLASH_LOAN_ARB: u64 = 450_000;
}

pub struct GasOracle {
    ewm_gas_price: f64,
    alpha: f64,
    count: u64,
    m2: f64,
    eth_price_usd: f64,
}

impl GasOracle {
    pub fn new(initial_gwei: f64, alpha: f64, eth_price_usd: f64) -> Self {
        assert!((0.0..=1.0).contains(&alpha), "alpha must be in (0, 1)");
        Self {
            ewm_gas_price: initial_gwei,
            alpha,
            count: 1,
            m2: 0.0,
            eth_price_usd,
        }
    }

    pub fn update(&mut self, observed_gwei: f64) {
        self.ewm_gas_price = self.alpha * observed_gwei + (1.0 - self.alpha) * self.ewm_gas_price;

        self.count += 1;
        let delta = observed_gwei - self.ewm_gas_price;
        let delta2 = observed_gwei - self.ewm_gas_price;
        self.m2 += delta * delta2;
    }
    pub fn std_dev(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        (self.m2 / (self.count - 1) as f64).sqrt()
    }

    pub fn pessimistic_estimate(&self) -> f64 {
        self.ewm_gas_price + 1.5 * self.std_dev()
    }

    pub fn current_gwei(&self) -> f64 {
        self.ewm_gas_price
    }

    pub fn set_eth_price(&mut self, eth_price_usd: f64) {
        self.eth_price_usd = eth_price_usd;
    }

    pub fn estimated_cost_usd(&self, gas_units: u64) -> f64 {
        let gwei = self.pessimistic_estimate();
        gas_units as f64 * gwei * 1e-9 * self.eth_price_usd
    }

    pub fn net_profit_usd(&self, gross_profit_usd: f64, gas_units: u64) -> (f64, f64) {
        let gas_cost = self.estimated_cost_usd(gas_units);
        (gross_profit_usd - gas_cost, gas_cost)
    }
}

impl Default for GasOracle {
    fn default() -> Self {
        Self::new(30.0, 0.1, 3_000.0)
    }
}

pub const MIN_NET_PROFIT_USD: f64 = 0.50;

pub const MAX_GAS_PROFIT_RATIO: f64 = 0.50;

pub fn safety_check_with_gas(
    gross_profit_usd: f64,
    gas_units: u64,
    oracle: &GasOracle,
) -> Result<f64, String> {
    let (net, gas_cost) = oracle.net_profit_usd(gross_profit_usd, gas_units);

    if net < MIN_NET_PROFIT_USD {
        return Err(format!(
            "Net profit ${:.3} below floor ${:.2} after gas cost ${:.3} ({:.1} gwei pessimistic)",
            net,
            MIN_NET_PROFIT_USD,
            gas_cost,
            oracle.pessimistic_estimate(),
        ));
    }

    if gross_profit_usd > 0.0 {
        let ratio = gas_cost / gross_profit_usd;
        if ratio > MAX_GAS_PROFIT_RATIO {
            return Err(format!(
                "Gas ${:.3} consumes {:.1}% of gross profit ${:.3} (max {:.0}%)",
                gas_cost,
                ratio * 100.0,
                gross_profit_usd,
                MAX_GAS_PROFIT_RATIO * 100.0,
            ));
        }
    }

    Ok(net)
}
