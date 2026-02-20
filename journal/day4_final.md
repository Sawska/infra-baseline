## Day 4 — February 20, 2026

### Numbers

* **Starting capital:** ~$71.00
* **Ending capital:** ~$71.00
* **PnL:** $0.00
* **Trades:** 0 completed (Multiple attempts blocked safely by inventory checks)
* **Win rate:** 0%
* **Best trade:** $0.00
* **Worst trade:** $0.00 (No unwinds! No gas wasted!)
* **Fees paid:** $0.00 (CEX) + $0.00 (DEX gas)

### What Happened

Today was all about testing the new safety architecture and the `$5` micro-trade configurations. The bot successfully scanned both `PEPE/USDT` and `WETH/USDT`. It successfully identified highly profitable spreads for WETH (e.g., spreads of 50-55 bps, well above the 20 bps minimum).

However, because I hadn't wrapped my ETH into WETH on the Arbitrum network yet, the bot correctly identified the missing inventory (`DEX WETH available: 0.00`) and aborted the signals *before* sending them to the blockchain. Later in the session, the bot triggered the Kill Switch because the hourly trade limit counter reached 20 (counting the safe aborts), and it went into suspension. I lost absolutely nothing today.

### Problems Encountered

1. **Missing WETH Inventory:** The bot found great `BuyCexSellDex` opportunities for WETH, but it couldn't execute them because I didn't have the `0.01` Wrapped ETH (WETH) on Arbitrum required for the DEX sell leg.
2. **Aggressive Hourly Limiter:** The bot's `max_trades_per_hour: 20` limit counts *failed/aborted attempts* as trades. Because it kept trying and aborting the WETH trade safely, it hit the limit of 20 and triggered the Kill Switch unnecessarily.
3. **Binance API Timeouts:** Toward the end of the logs, there were several `error sending request for url` serialization errors, indicating a connection issue or rate-limiting block from the Binance API.

### Changes Made

* **Scaled Down to $5 Trades:** Updated `PerPairConfig` in `main.rs` to set `trade_size: 1_250_000.0` for PEPE and `trade_size: 0.0026` for WETH to drastically lower risk exposure per trade.
* **Capital Preservation Logic:** Relied on the newly implemented inventory pre-checks. Instead of blind execution and painful unwinds, the bot now successfully uses the `inventory_ok = false` flag to mark signals invalid safely.

### Lessons Learned

* **Defense works!** Yesterday's unwinds cost $14.00. Today, the exact same scenario (missing inventory) cost $0.00 because the pre-trade validation caught it in time.
* **Counters need context.** The Hourly Trade Limit should ideally only count *executed* transactions to the blockchain. Counting safe pre-trade aborts causes the bot to shut down its own monitoring too early.
* **WETH is distinct from ETH.** To trade on Uniswap pools, raw ETH must be manually converted (wrapped) into the ERC-20 WETH token first.
Based on your daily journals, logs, and reflections, here is your Final Report. It perfectly captures the transition from theoretical coding to the brutal, yet incredibly valuable, reality of live market execution.

---

# Part 6: Final Report - Algorithmic Arbitrage Bot

## 1. Configuration & Setup

* **Chain:** Arbitrum Mainnet
* **DEX:** Uniswap V3 (and other Arbitrum AMMs)
* **CEX:** Binance
* **Pairs Traded:** Explored ARB/USDT, WBTC/USDT. Settled on **PEPE/USDT** (for volatility/testing) and **WETH/USDT** (for stability).
* **Risk Parameters Chosen:** * *Trade Size:* $5–$10 equivalents (e.g., 1,250,000 PEPE or 0.0026 WETH) to minimize capital risk while satisfying CEX `MIN_NOTIONAL` limits.
* *Min Spread:* 130 bps for PEPE (to cover its higher 0.3% pool fees and volatility), 20 bps for WETH (cheaper 0.05% pools).
* *Limits:* Max daily loss of $10, max drawdown 15%, max 20 trades per hour.


* **Testnet to Production Surprises:** The biggest surprise was **API strictness and decimal precision**. On Testnet, sending a price of `0.00000418342` works fine. On Production, Binance immediately throws a `-1013 Invalid price` error because it strictly enforces `TICK_SIZE` rounding. Furthermore, real L2 gas isn't "free"—even failed or reverted transactions cost $0.02 each, which rapidly bleeds a small account.

## 2. Trading Results

* **Starting Capital:** $100.00
* **Ending Capital:** ~$71.00
* **Total PnL:** -$29.00
* **Total Trades:** Dozens of execution attempts, 1 fully completed arbitrage cycle.
* **Win Rate:** 100% for completed cycles (1/1), 0% for unwound/aborted attempts.
* **Best Trade:** +$0.05 (Net profit on a `BuyDexSellCex` PEPE/USDT trade on Day 3).
* **Worst Trade:** The "Unwind Bleed" sequence. The bot bought PEPE on the DEX, but the CEX sell order failed due to decimal formatting. To protect the portfolio, the bot executed an "unwind" (selling the PEPE back on the DEX). I paid gas twice, pool fees twice, and suffered slippage, entirely wiping out the potential profit.
* **Total Fees Paid:** ~$0.00 on CEX (due to mostly rejected orders). ~$29.00 on DEX (gas for failed transactions, gas for unwinds, router swap fees for manual rebalancing, and inventory market depreciation).

**Anatomy of the $29 Loss (How the money was actually lost):**
I did not lose $29 because the arbitrage math was wrong. I lost it due to three operational realities:

1. **The Unwind Bleed:** When Binance rejected orders, the bot was forced to sell DEX tokens back to the pool at a loss.
2. **Inventory Risk:** Arbitrage requires "Double Inventory" (holding assets on both exchanges). I held $WBTC and 1.5 million PEPE on spot. While I was debugging the bot, the market value of PEPE and WBTC crashed. I lost fiat value simply by holding volatile assets as inventory.
3. **Manual Rebalancing & Gas:** Moving funds around to "fix" skewed balances after failed trades cost Arbitrum network gas and DEX router fees every single time.

## 3. Risk Management in Practice

* **Circuit Breaker:** **Tripped.** On Day 2 and 3, when Binance kept throwing `-1013` errors, the Executor's Circuit Breaker opened, successfully pausing the `tick()` loop to prevent the bot from spamming the exchange and getting my API key banned.
* **Kill Switch:** **Activated.** The Hourly Trade Limit (20) successfully triggered on Day 3 when the bot got caught in a loop of trying to execute and reverting. The "Watchdog" balance mismatch also triggered the Kill Switch when my expected internal balances deviated from the blockchain reality.
* **The Scariest Moment:** The **"$1.5 Million ETH" Bug**. Due to a `HashMap` string mismatch in my config, the bot defaulted to PEPE's trade size (1,500,000) while evaluating WETH. The bot calculated a risk size of 1.5 million ETH.
* **Risk Control that Saved Me:** The `RiskManager.check_pre_trade()` instantly blocked the 1.5M ETH trade. On Day 4, the **Inventory Pre-Check** successfully halted trades *before* execution because I had 0.00 WETH, saving me from another painful unwind bleed.

## 4. What I Learned

* **Biggest Surprise:** Arbitrage is **inventory synchronization**. You do not buy on a DEX and transfer to a CEX. You must already hold the token on the CEX to sell instantly. This completely changes the risk profile because you are constantly exposed to the underlying asset's market price.
* **What I would do differently with $1,000:** 1. **No Meme Coins:** I would strictly trade highly liquid, stable pairs like WETH/USDT or ARB/USDT to eliminate inventory depreciation risk.
2. **Larger Size:** Fixed L2 gas fees ($0.02) eat 100% of the profit on a $5 trade. With $1k, I would size trades at $100–$200, making gas fees mathematically irrelevant compared to the gross spread.
* **Confidence Levels:** I am highly confident in my architecture (Risk limits, Kill Switches, Event Loops). They worked flawlessly to stop bad things from happening. I am least confident in Exchange API formatting (handling floating-point math, `LOT_SIZE`, and `TICK_SIZE`).
* **Wish I Built Earlier:** A robust, exchange-specific **Data Normalization / Rounding Engine**. Throwing raw Rust `f64` floats at Binance is a recipe for disaster.

## 5. Technical Challenges

* **L2 Adaptation Issues:** Decentralized exchanges do not trade native ETH; they trade wrapped ERC-20 WETH. Failing to understand that I needed to manually wrap my ETH into WETH caused "Insufficient Funds" errors even though my wallet had gas money.
* **Gas & Slippage Estimation:** Arbitrum is fast, but meme coin volatility is faster. My initial 0.5% slippage tolerance caused transactions to revert with "Too little received." I had to increase slippage to 5% to guarantee execution on PEPE.
* **Latency (CEX vs DEX):** Binance order books update in milliseconds; Arbitrum blocks take ~1-2 seconds. This desync means a spread can vanish between the time the bot reads the CEX order book and the time the DEX smart contract executes.
* **Production-Only Bugs:** * Trying to execute `Emergency Flatten` on 4,254 PEPE (worth $0.02). Binance rejected it because spot orders have a hard $5.00 `MIN_NOTIONAL` limit. This dust got stuck in my wallet.
* The realization that counting safe "pre-trade aborts" toward the `max_trades_per_hour` limit will unnecessarily trigger the Kill Switch, as seen on Day 4.
