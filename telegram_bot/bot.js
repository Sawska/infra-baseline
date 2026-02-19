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
    ctx.reply("▶️ Kill switch cleared. Bot resuming.");
    console.log("[Telegram] /resume received");
});

bot.command('status', (ctx) => {
    const now = Date.now();
    let statusMsg = "🔴 Offline / Unknown";
    let lastSeenText = "Never";

    if (lastSeen > 0) {
        const diffSeconds = Math.floor((now - lastSeen) / 1000);
        lastSeenText = `${diffSeconds}s ago`;

        if (diffSeconds < 65) {
            statusMsg = "🟢 Online & Healthy";
        } else {
            statusMsg = "⚠️ Unresponsive (Hanging?)";
        }
    }

    const killSwitchStatus = stopFlag ? "ON (Paused)" : "OFF (Active)";

    ctx.reply(
        `🤖 **Bot Status Report**\n` +
        `-----------------------\n` +
        `Status: ${statusMsg}\n` +
        `Last Heartbeat: ${lastSeenText}\n` +
        `Kill Switch: ${killSwitchStatus}\n` +
        `Port: ${PORT}`
    );
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
