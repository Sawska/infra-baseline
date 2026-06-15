-- Equity snapshots: live CEX + on-chain wallet balances valued in USD, captured
-- periodically by the bot so the account equity curve can be charted alongside
-- realised trade PnL in arb_trades.
CREATE TABLE IF NOT EXISTS balance_snapshots (
    id          BIGSERIAL   PRIMARY KEY,
    timestamp   TIMESTAMPTZ NOT NULL,
    cex_usd     NUMERIC     NOT NULL,
    wallet_usd  NUMERIC     NOT NULL,
    total_usd   NUMERIC     NOT NULL,
    breakdown   TEXT        NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_balance_snapshots_timestamp ON balance_snapshots (timestamp);
