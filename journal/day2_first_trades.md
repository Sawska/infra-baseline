## Day 2 — February 18, 2026

### Numbers

* **Starting capital:** $100.00
* **Ending capital:** ~$85.00 (Estimated due to gas fees, failed tx costs, and slippage)
* **PnL:** -$15.00 (Mostly gas fees for failed transactions and initial setup costs)
* **Trades:** 0 completed full cycles (Many partial executions/reverts)
* **Win rate:** 0%
* **Best trade:** None (Closest was the PEPE DEX buy that executed but couldn't sell on CEX)
* **Worst trade:** Failed DEX swap on PEPE costing ~$0.02 in gas without execution.
* **Fees paid:** ~$0.00 (CEX - no fills) + ~$0.30 (DEX gas for approves & failed txs)

### What Happened

Today was a massive pivot from "Dry Run" to "Production." I traversed multiple pairs—**ARB/USDT**, **WBTC/USDT**, and **ETH/USDT**—using CoinMarketCap to manually verify volume and liquidity before settling on **PEPE/USDT** as the best volatility candidate.

* **Pair Traversal:**
* **ARB/USDT & WBTC/USDT:** Configured these initially but abandoned them due to low volatility or high capital requirements for `MIN_NOTIONAL` tests.
* **PEPE/USDT:** Selected for high volatility and low unit price, perfect for testing micro-trades ($6–$10).


* **Analysis:** Used Arbiscan heavily to decode generic "execution reverted" errors into specific actionable insights (e.g., "Too little received").

### Problems Encountered

1. **The "Gas" Hurdle:** The bot threw `RpcError: insufficient funds` immediately. I had USDT but zero ETH on Arbitrum.
2. **Slippage Reverts (Uniswap V3):** Transactions failed with "Too little received." My default 0.5% tolerance was too tight for PEPE's volatility.
3. **The "$1.5 Million" Bug:** A critical logic error where the bot tried to send `1,500,000 USDT` to buy PEPE instead of sending `$6` worth of USDT. This caused a massive revert.
4. **Double Inventory Block:** I successfully bought PEPE on Dex, but the CEX leg failed because I didn't have PEPE on Binance to sell instantly.
5. **Binance `MIN_NOTIONAL`:** Orders were rejected because of a math error where the bot rounded the tiny PEPE price (`0.00000424`) down to `0.00` because the tick size was set for ETH (`0.01`).

### Changes Made

* **Infrastructure:** Deposited ~$10 ETH to Arbitrum wallet for gas.
* **Inventory:** Bought ~5.9M PEPE on Binance manually to enable the "Sell CEX" leg.
* **Config - Slippage:** Increased `slippage_tolerance` to **5% (0.05)** to ensure execution on volatile meme coins.
* **Config - Timeouts:** Increased Binance timeout to **20s** and Uniswap to **30s** to prevent premature unwinds.
* **Code - Input Logic:** Rewrote `execute_dex_leg` to correctly calculate USDT input: `input = size * price` instead of just `size`.
* **Code - Rounding:** Changed `PRICE_TICK` from `0.01` to `0.00000001` so the bot can handle sub-penny assets like PEPE.

### Lessons Learned

* **Validate Input Units:** Never assume "amount" means "token count." On a buy leg, "amount" means "USDT cost."
* **Inventory Management:** Arbitrage isn't teleportation. You must have assets on *both* sides (Double Inventory) to trade instantaneously.
* **Math Matters:** Generic rounding settings (like for ETH) break completely when applied to meme coins with 6-8 decimal places.

### Tomorrow's Plan

* **Force Profit:** Lower `min_profit_usd` to **$0.01** to force the bot to complete its first full successful cycle, proving the system works.
* **Scale Up:** Once the first cycle confirms stability, increase trade size to **$15-$20** to overcome the fixed ~$0.02 gas fee and achieve actual profitability.
* **Visualization:** Implement the PnL charting HTML dashboard to visualize performance in real-time.
