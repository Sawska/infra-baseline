#!/bin/bash
HEARTBEAT_FILE="/tmp/arb_bot_heartbeat"
KILL_SWITCH_FILE="/tmp/arb_bot_kill"
TIMEOUT_SEC=30

echo "🐶 Watchdog started. Monitoring $HEARTBEAT_FILE..."

while true; do
    if [ -f "$HEARTBEAT_FILE" ]; then
        current_time=$(date +%s)
        last_update=$(cat "$HEARTBEAT_FILE")
        diff=$((current_time - last_update))

        if [ "$diff" -gt "$TIMEOUT_SEC" ]; then
            echo "🚨 ALARM: Bot heartbeat lost for $diff seconds!"
            touch "$KILL_SWITCH_FILE"
        else
            echo "✅ Heartbeat OK (Last update: $diff sec ago)"
        fi
    else
        echo "⚠️  Heartbeat file not found yet..."
    fi
    sleep 5
done
