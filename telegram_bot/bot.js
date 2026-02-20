const express = require('express');
const { Telegraf } = require('telegraf');
require('dotenv').config();

const PORT = process.env.PORT || 3000;
const BOT_TOKEN = process.env.TELEGRAM_BOT_TOKEN;
const CHAT_ID = process.env.TELEGRAM_CHAT_ID;

if (!BOT_TOKEN) {
    console.error("TELEGRAM_BOT_TOKEN is required");
    process.exit(1);
}

const app = express();
app.use(express.json());

const bot = new Telegraf(BOT_TOKEN);

let stopFlag = false;
let lastUpdateId = 0;
let lastSeen = 0;
let botState = null; // Caches the rich metrics from the rust bot

bot.start((ctx) => {
    ctx.reply(
        "Bridge active.\n\nCommands:\n" +
        "/stop - activate kill switch\n" +
        "/resume - clear kill switch\n" +
        "/status - show bridge & bot status"
    );
});

bot.command('stop', (ctx) => {
    stopFlag = true;
    lastUpdateId++;
    ctx.reply("🛑 Kill switch activated. Bot should pause shortly.");
    console.log("[Telegram] /stop received");
});

bot.command('resume', (ctx) => {
    stopFlag = false;
    ctx.reply("▶️ Bridge kill switch cleared. \n\nBot will resume trading IF internal safety limits (like error counts) allow it.");
    console.log("[Telegram] /resume received");
});

bot.command('status', (ctx) => {
    const now = Date.now();
    let statusMsg = "🔴 Offline / Unknown";
    let lastSeenText = "Never";
    let isOnline = false;

    if (lastSeen > 0) {
        const diffSeconds = Math.floor((now - lastSeen) / 1000);
        lastSeenText = `${diffSeconds}s ago`;

        if (diffSeconds < 90) {
            isOnline = true;
            statusMsg = "🟢 Online & Healthy";
        } else {
            statusMsg = "⚠️ Unresponsive (Hanging?)";
        }
    }

    const killSwitchStatus = stopFlag ? "ON (Paused)" : "OFF (Active)";

    if (!botState || !isOnline) {
        // Fallback display if we don't have rich data yet
        return ctx.reply(
            `🤖 <b>Bot Status Report</b>\n` +
            `━━━━━━━━━━━━━━━━━━━━━━\n` +
            `Status: ${statusMsg}\n` +
            `Last Heartbeat: ${lastSeenText}\n` +
            `Kill Switch: ${killSwitchStatus}\n` +
            `Port: ${PORT}\n\n` +
            `<i>(Waiting for next 60s sync to display full PnL dashboard...)</i>`,
            { parse_mode: 'HTML' }
        );
    }

    const mode = botState.dry_run ? "🧪 Dry Run" : "⚡ Live Trading";
    const active = botState.trading_active ? "▶️ Active" : "⏸️ Paused";

    const formatMoney = (val) => val >= 0 ? `+$${val.toFixed(2)}` : `-$${Math.abs(val).toFixed(2)}`;

    const winRate = botState.trades_today > 0
        ? ((botState.wins / botState.trades_today) * 100).toFixed(1)
        : "0.0";

    const h = Math.floor(botState.uptime_secs / 3600);
    const m = Math.floor((botState.uptime_secs % 3600) / 60);
    const uptimeStr = `${h}h ${m}m`;

    const msg = `📊 <b>Arbitrage Bot Status</b>
━━━━━━━━━━━━━━━━━━━
${active} | ${mode}
⏱️ <b>Uptime:</b> ${uptimeStr}
📡 <b>Last Sync:</b> ${lastSeenText}

💰 <b>Performance</b>
• Session PnL: ${formatMoney(botState.session_pnl)}
• Total PnL: ${formatMoney(botState.cumulative_pnl)}
• Capital: $${botState.capital.toFixed(2)}
• Drawdown: ${botState.drawdown.toFixed(2)}%

📈 <b>Trades Today</b>
• Total: ${botState.trades_today} (${botState.wins}W / ${botState.losses}L)
• Win Rate: ${winRate}%

🔌 <b>System</b>
• Kill Switch (Node): ${killSwitchStatus}
━━━━━━━━━━━━━━━━`;

    ctx.reply(msg, { parse_mode: 'HTML' });
});

bot.launch().then(() => {
    console.log("Telegraf bot running");
});

app.post("/sendMessage", async (req, res) => {
    lastSeen = Date.now();
    const { chat_id, text, parse_mode } = req.body;
    try {
        const result = await bot.telegram.sendMessage(chat_id, text, { parse_mode });
        res.json({ ok: true, result });
    } catch (err) {
        console.error("Send error:", err.message);
        res.status(500).json({ ok: false, description: err.message });
    }
});

app.post("/updateState", (req, res) => {
    lastSeen = Date.now();
    botState = req.body;
    res.json({ ok: true });
});

app.get("/getUpdates", (req, res) => {
    lastSeen = Date.now();

    const updates = [];
    const command = stopFlag ? "/stop" : "/resume";

    updates.push({
        update_id: lastUpdateId,
        message: {
            message_id: Date.now(),
            chat: { id: Number(CHAT_ID), type: "private" },
            date: Math.floor(Date.now() / 1000),
            text: command
        }
    });

    res.json({ ok: true, result: updates });
});

app.listen(PORT, () => {
    console.log(`Bridge listening on http://localhost:${PORT}`);
});

process.once('SIGINT', () => bot.stop('SIGINT'));
process.once('SIGTERM', () => bot.stop('SIGTERM'));
