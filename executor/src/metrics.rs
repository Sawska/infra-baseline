use prometheus::{Counter, Encoder, Histogram, HistogramOpts, Opts, TextEncoder};

#[derive(Clone)]
pub struct ExecutionMetrics {
    pub execution_attempts: Counter,
    pub execution_success: Counter,
    pub execution_failure: Counter,
    pub leg_cex_failures: Counter,
    pub leg_dex_failures: Counter,
    pub net_pnl: Histogram,
}

impl ExecutionMetrics {
    pub fn new() -> Self {
        Self {
            execution_attempts: Self::register_counter_safe(
                "arb_execution_attempts_total",
                "Total number of arbitrage execution attempts",
            ),
            execution_success: Self::register_counter_safe(
                "arb_execution_success_total",
                "Total number of successful arbitrage executions",
            ),
            execution_failure: Self::register_counter_safe(
                "arb_execution_failure_total",
                "Total number of failed arbitrage executions",
            ),
            leg_cex_failures: Self::register_counter_safe(
                "arb_leg_cex_failures_total",
                "Total number of CEX leg failures",
            ),
            leg_dex_failures: Self::register_counter_safe(
                "arb_leg_dex_failures_total",
                "Total number of DEX leg failures",
            ),
            net_pnl: Self::register_histogram_safe(
                "arb_execution_pnl_usd",
                "Distribution of Net PnL in USD",
                vec![0.0, 1.0, 5.0, 10.0, 50.0, 100.0],
            ),
        }
    }

    fn register_counter_safe(name: &str, help: &str) -> Counter {
        let counter = Counter::with_opts(Opts::new(name, help)).unwrap();
        let _ = prometheus::register(Box::new(counter.clone()));
        counter
    }

    fn register_histogram_safe(name: &str, help: &str, buckets: Vec<f64>) -> Histogram {
        let histogram =
            Histogram::with_opts(HistogramOpts::new(name, help).buckets(buckets)).unwrap();
        let _ = prometheus::register(Box::new(histogram.clone()));
        histogram
    }
}

impl Default for ExecutionMetrics {
    fn default() -> Self {
        Self::new()
    }
}

pub fn gather_metrics() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();

    match encoder.encode(&metric_families, &mut buffer) {
        Ok(_) => String::from_utf8(buffer)
            .unwrap_or_else(|_| String::from("# Error: UTF-8 conversion failed")),
        Err(e) => format!("# Error: {}", e),
    }
}
