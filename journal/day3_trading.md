## Day 3 — February 19, 2026

### Numbers

* **Starting capital:** ~$85.00
* **Ending capital:** ~$71.00
* **PnL:** -$14.00 (Gas fees for unwinds, manual rebalancing costs, and PEPE inventory depreciation)
* **Trades:** 1 completed full cycle (Dozens of failed/unwound attempts)
* **Win rate:** 100% (for completed cycles), 0% for unwound attempts
* **Best trade:** +$0.05 (Net profit on a `BuyDexSellCex` PEPE trade)
* **Worst trade:** The "unwind bleed" — a series of transactions where the DEX leg succeeded but the CEX leg failed, forcing the bot to sell back the DEX tokens at a guaranteed loss.
* **Fees paid:** ~$0.00 (CEX - mostly rejected) + ~$19.00 (DEX gas for unwinds, manual swap router fees, and market loss on holding PEPE)

### What Happened

A day of massive breakthroughs and painful realities. **The bot successfully executed its first fully profitable arbitrage cycle!** It identified a 206 bps spread, bought 1.5 million PEPE on Arbitrum, and simultaneously sold it on Binance for a net profit of $0.05. The core architecture *works*.

However, right after the success, I suffered a $19 bleed. The successful trade skewed my inventory (my PEPE moved to the DEX, and USDT to the CEX). When the bot tried to capture the spread again, Binance rejected the orders due to formatting errors (`-1013 Invalid price`). Because the CEX leg failed, the bot's safety mechanism kicked in and "unwound" the trades—selling the PEPE back to the DEX pool. This meant I paid gas and swap fees *twice* for a failed trade. Combined with the manual fees to rebalance my wallets and PEPE dropping in market value, it cost me $19 to learn this lesson.

### Problems Encountered

1. **The Unwind Bleed:** When Binance rejected an order, the bot had to revert the DEX leg. Unwinding is incredibly expensive because it guarantees a loss on gas and the spread.
2. **Binance API `-1013 Invalid price`:** Binance strictly rejects prices with too many decimal places (e.g., `0.00000418342`). The bot was sending raw floats without rounding to the exact `TICK_SIZE`.
3. **Binance `LOT_SIZE` Error:** During a Kill Switch trigger, the "Emergency Flatten" tried to sell 4,254 PEPE (~$0.02). Binance rejected it because spot orders must be at least $5.00 in total notional value.
4. **The "1.5 Million ETH" Bug:** Because I added WETH/USDT but had a string mismatch in the config keys, the bot defaulted to the PEPE config and tried to trade 1.5 million ETH, causing immediate safety blocks.
5. **Inventory Risk:** Holding 1.5 million PEPE as base inventory means I am exposed to PEPE's price action. Part of the $19 loss was simply the PEPE token losing value in my wallet while I was testing.

### Changes Made

* **Dynamic Configuration (`PerPairConfig`):** Refactored the bot to hold a `HashMap` of specific configs for each pair. Now PEPE trades at 1.5M size with a 130 bps spread requirement, while WETH trades at 0.005 size with a 20 bps requirement.
* **Generator "Hot-Swapping":** Added `update_config()` and `set_fees()` to the `SignalGenerator` so it can dynamically switch its math (e.g., 0.3% fee for PEPE vs 0.05% for WETH) inside the `tick()` loop.
* **Watchdog Verification:** Wrote `verify_balances` to actively cross-reference the CEX/DEX reality with the bot's internal inventory memory. If it mismatches by >0.001, it halts.
* **Graceful Suspension:** Replaced the hard process `break` with a `suspended = true` state. Now, if the bot hits a risk limit or inventory error, it pauses trading but keeps the metrics server, logging, and heartbeat alive so the bash watchdog doesn't unnecessarily restart it.

### Lessons Learned

* **Unwinds are the enemy.** If the CEX leg is brittle, the DEX leg becomes a liability. CEX formatting (price ticks, lot sizes) must be absolutely flawless before sending the transaction.
* **Meme coins are a double-edged sword.** The volatility creates massive spreads (200+ bps), but holding them as inventory introduces severe market depreciation risks that can wipe out hard-earned arbitrage gains.
* **Dynamic configs are mandatory.** You cannot treat a $2,000 ETH token and a $0.000004 PEPE token with the same generic math and sizing logic.

### Tomorrow's Plan

* **Strict CEX Formatting:** Implement robust rounding logic for both Price (`TICK_SIZE`) and Quantity (`LOT_SIZE`, `MIN_NOTIONAL`) in the Binance exchange client to eliminate `-1013` errors forever.
* **Back to Dry Run:** Keep the bot in `dry_run: true` until the new Binance rounding logic is visually confirmed in the logs. No more unwinds!
