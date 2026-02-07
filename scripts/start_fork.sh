#!/bin/bash
set -e

if [ -f .env ]; then
    echo "Loading variables from .env..."
    export $(grep -v '^#' .env | xargs)
fi

if [ -z "$ETH_RPC_URL" ]; then
    echo "Error: ETH_RPC_URL is not set. Please add it to your .env file."
    exit 1
fi

echo "Starting Anvil fork from: $ETH_RPC_URL"

anvil \
    --fork-url "$ETH_RPC_URL" \
    --port 8545 \
    --accounts 10 \
    --balance 10000 \
    >/tmp/anvil.log 2>&1 &

ANVIL_PID=$!

echo "Waiting for Anvil to be ready..."
until curl -s http://127.0.0.1:8545 >/dev/null; do
    sleep 0.5
done

echo "Anvil is ready (pid=$ANVIL_PID)"
