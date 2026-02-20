# Part 6: Final Report - Algorithmic Arbitrage Bot Post-Mortem

## Executive Summary

This report encapsulates the culmination of a multi-week engineering initiative to build, deploy, and monitor an automated algorithmic arbitrage trading bot. The objective was to capture price inefficiencies between decentralized exchanges (DEXs) and centralized exchanges (CEXs). Transitioning the bot from a simulated "Dry Run" environment to a live production environment with real capital provided a masterclass in market microstructure, API strictness, and the absolute necessity of fault-tolerant risk management. What started as a $100 experiment ended at ~$71, resulting in a $29 "tuition fee" that exposed critical differences between theoretical arbitrage and physical execution.

---

## 1. Configuration & Setup

### Infrastructure Decisions

* **Chain:** Arbitrum Mainnet. Selected for its optimistic rollup technology, providing the high transaction throughput and sub-cent gas fees necessary for micro-arbitrage, which would be impossible on Ethereum Layer 1.
* **DEX:** Uniswap V3 (and other local Arbitrum AMMs). Chosen for deep liquidity pools and the ability to route via specific fee tiers (0.05% to 0.3%).
* **CEX:** Binance. Selected as the centralized counterparty due to its industry-leading order book depth and millisecond API response times.

### Asset Selection

During the testing phase, I explored multiple pairs including ARB/USDT and WBTC/USDT. However, for live production, the strategy bifurcated into two distinct pairs for different testing purposes:

1. **PEPE/USDT:** Selected specifically for its extreme volatility and low unit price. It was the perfect candidate for stress-testing micro-trades ($5–$10 equivalents) to force the bot into frequent execution cycles.
2. **WETH/USDT:** Selected later in the week for stability, aiming to reduce the delta exposure (inventory risk) associated with holding meme coins.

### Risk Parameters

* **Trade Sizing:** Configured to execute $5–$10 equivalents per cycle (e.g., 1,250,000 PEPE or 0.0026 WETH). This was specifically calculated to minimize capital risk while remaining just above the Binance spot market `MIN_NOTIONAL` limit (which rejects orders under $5.00).
* **Spread Thresholds:** Configured dynamically. PEPE required a minimum 130 bps spread to offset its 0.3% pool fees and high slippage, whereas WETH was tightened to a 20 bps threshold due to its cheaper 0.05% fee tier and deeper liquidity.
* **Hard Limits:** Max daily loss of $10, maximum drawdown of 15%, and a velocity limit of 20 trades per hour to prevent runaway execution loops.

### Surprises from Testnet to Production

The most glaring surprise was **API strictness and decimal precision**. On local testnets or simulations, passing a raw 64-bit float price of `0.00000418342` is mathematically accepted. In production, Binance immediately rejects these payloads with a `-1013 Invalid price` error. Real-world CEXs strictly enforce `TICK_SIZE` (price rounding) and `STEP_SIZE` (quantity rounding). Furthermore, the realization that L2 gas is not entirely "free"—even failed or reverted smart contract calls cost ~$0.02—completely changed the economic viability of $5 trades.

---

## 2. Trading Results & Capital Dynamics

* **Starting Capital:** $100.00
* **Ending Capital:** ~$71.00
* **Total PnL:** -$29.00
* **Total Trades:** Dozens of execution attempts, 1 fully completed atomic arbitrage cycle.
* **Win Rate:** 100% for completed cycles (1 win, 0 losses), but 0% for unwound/aborted attempts.
* **Best Trade:** +$0.05. A net profitable `BuyDexSellCex` PEPE/USDT trade executed perfectly on Day 3, proving the core mathematical and routing architecture works.
* **Worst Trade:** A sequence known as the "Unwind Bleed." The bot successfully bought PEPE on the DEX, but the simultaneous CEX sell order failed due to decimal formatting. To protect the portfolio from directional exposure, the bot executed an "unwind" (dumping the PEPE back into the DEX pool). I paid gas twice, pool fees twice, and suffered slippage, entirely wiping out the potential profit and eating into the principal.

### Anatomy of the $29 Loss

It is vital to note that the loss was *not* due to flawed arbitrage math (the bot accurately identified real spreads). The capital degradation was the result of three operational realities:

1. **The Unwind Bleed:** When Binance rejected orders, the bot was forced to sell DEX tokens back to the pool at a guaranteed loss. Unwinds are the silent killers of arbitrage bots.
2. **Inventory Risk (Delta Exposure):** Arbitrage requires holding inventory on both exchanges. To trade PEPE, I had to hold millions of PEPE tokens. During the testing days, the wider crypto market dipped, taking PEPE's value down with it. A significant portion of the $29 loss was simply fiat depreciation of my held inventory, completely independent of the bot's trading activity.
3. **Manual Rebalancing & Gas:** Moving funds around to "fix" skewed balances after failed trades cost Arbitrum network gas and DEX router swap fees every single time I reset the board.

---

## 3. Risk Management in Practice

The losses would have been catastrophic if not for the multi-layered defense architecture. The safety systems were tested repeatedly and functioned flawlessly.

* **Circuit Breaker:** **Tripped.** On Days 2 and 3, when Binance repeatedly threw `-1013` formatting errors, the Executor's Circuit Breaker opened. It successfully paused the `tick()` loop, preventing the bot from spamming the exchange with bad requests, which would have resulted in an IP ban from Binance.
* **Hourly Kill Switch:** **Activated.** The Hourly Trade Limit (20) successfully triggered on Day 3 when the bot got caught in a loop of attempting trades and immediately unwinding them. This physical cut-off stopped the "Unwind Bleed" from draining the entire $100 in a single afternoon.
* **The "Watchdog" Integrity Check:** **Activated.** This routine cross-references the internal code's expected balances with the actual blockchain/exchange reality. When my balances drifted due to manual interventions, the Watchdog detected a mismatch > 0.001 ETH, triggered the Kill Switch, and halted trading.

### The Scariest Moment

The most dangerous moment of the week was the **"$1.5 Million ETH" Bug**. Due to a simple string mismatch in my `HashMap` configuration keys, the bot fell back to the PEPE default trade size (1,500,000 units) while attempting to evaluate a WETH trade. The bot calculated a risk order to buy 1.5 million Ethereum. Fortunately, the `RiskManager.check_pre_trade()` function caught the monstrous USD equivalent, flagged it against the $10 maximum limit, and instantly blocked execution.

---

## 4. What I Learned

### The Core Paradigm Shift: Inventory Synchronization

The biggest conceptual breakthrough was realizing that arbitrage is strictly **inventory synchronization**. You do not buy an asset on a DEX, wait for a blockchain transfer, and sell it on a CEX. You must *already hold* the base asset on the CEX to sell instantly, and hold quote stablecoins on the DEX to buy instantly.

This entirely changes the risk profile. Because you are maintaining "Double Inventory," you act as a long-term holder of the asset, exposing yourself to standard market risk (inventory depreciation).

### How I Would Treat $1,000

If capitalized with $1,000 rather than $100, my approach would change drastically:

1. **Strictly Stable/Major Pairs:** I would abandon meme coins (PEPE) entirely to eliminate severe inventory depreciation risk. I would deploy the capital exclusively into WETH/USDC or ARB/USDT.
2. **Scale the Trade Size:** Fixed Arbitrum L2 gas fees ($0.02) eat nearly 100% of the gross profit on a $5 micro-trade. With $1k, I would size trades at $100–$200. At that size, the fixed $0.02 gas fee becomes mathematically irrelevant, allowing the bot to keep the vast majority of the captured spread.

### Confidence Matrix

* **Most Confident:** The system architecture (Event loops, State suspension, Risk limits, Kill Switches). The bot demonstrated a highly robust immune system. It knew exactly when to stop itself.
* **Least Confident:** Exchange API specific formatting. Handling floating-point math, applying `LOT_SIZE` flooring, and formatting `TICK_SIZE` precision correctly in Rust proved to be an ongoing battle.
* **Missing Tooling:** I deeply wish I had built a robust, centralized **Data Normalization Engine** in Weeks 1-4. A middleware service that intercepts raw numbers and perfectly formats them for the destination exchange *before* they reach the Executor would have prevented all unwind bleeds.

---

## 5. Technical Challenges

* **L2 Adaptation Issues (Wrapped Assets):** Decentralized exchanges do not trade native ETH; they use ERC-20 smart contracts, requiring Wrapped ETH (WETH). Failing to understand this distinction initially caused "Insufficient Funds" errors on the DEX leg, even though my wallet had plenty of native ETH for gas. I had to manually interact with the Arbitrum WETH contract to prep the inventory.
* **Gas & Slippage Estimation vs. Volatility:** Arbitrum is incredibly fast, but meme coin volatility is faster. My initial 0.5% slippage tolerance on Uniswap V3 caused transactions to revert with "Too little received." I had to aggressively increase slippage to 5% to guarantee execution on PEPE, which inherently eats into the profit margin.
* **The Latency Desync (CEX vs DEX):** Centralized exchange order books update via WebSocket in milliseconds. Arbitrum blocks take ~1-2 seconds to process. This temporal desync means a highly profitable spread can vanish between the exact millisecond the bot reads the CEX order book and the moment the DEX smart contract executes the trade.
* **Production-Only Dust Bugs:** When the Kill Switch initiated an `Emergency Flatten` to convert everything to USDT, it tried to sell 4,254 PEPE (worth ~$0.02). Binance rejected the market order because spot markets enforce a hard $5.00 `MIN_NOTIONAL` limit. This resulted in "dust" getting permanently stuck in the Binance wallet, breaking the bot's assumption that it could truly liquidate to zero. Furthermore, the realization that counting safe, pre-execution aborts toward the `max_trades_per_hour` limit will unnecessarily trigger the Kill Switch was a behavior only discovered through live production monitoring.
