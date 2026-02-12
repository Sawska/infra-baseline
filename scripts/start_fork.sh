#!/bin/bash
set -e

# Load .env variables
if [ -f .env ]; then
    echo "Loading variables from .env..."
    export $(grep -v '^#' .env | xargs)
fi

if [ -z "$ETH_RPC_URL" ]; then
    echo "Error: ETH_RPC_URL is not set. Please add it to your .env file."
    exit 1
fi

echo "Starting Anvil fork from: $ETH_RPC_URL"

# Run Anvil in the foreground
anvil \
    --fork-url "$ETH_RPC_URL" \
    --port 8545 \
    --accounts 10 \
    --balance 10000
