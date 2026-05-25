use sqlx::postgres::{PgPool, PgPoolOptions};
use std::error::Error;

pub async fn init_db(database_url: &str) -> Result<PgPool, Box<dyn Error>> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS arb_trades (
            id              TEXT        PRIMARY KEY,
            timestamp       TIMESTAMPTZ NOT NULL,
            symbol          TEXT        NOT NULL,
            buy_venue       TEXT        NOT NULL,
            buy_amount      NUMERIC     NOT NULL,
            buy_price       NUMERIC     NOT NULL,
            buy_fee         NUMERIC     NOT NULL,
            buy_fee_asset   TEXT        NOT NULL,
            sell_venue      TEXT        NOT NULL,
            sell_amount     NUMERIC     NOT NULL,
            sell_price      NUMERIC     NOT NULL,
            sell_fee        NUMERIC     NOT NULL,
            sell_fee_asset  TEXT        NOT NULL,
            gas_cost_usd    NUMERIC     NOT NULL,
            net_pnl         NUMERIC     NOT NULL,
            bps             NUMERIC     NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_arb_trades_timestamp ON arb_trades (timestamp);
        "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}
