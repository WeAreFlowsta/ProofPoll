#!/bin/bash

# Build script for ProofPoll DNA v1.1
#
# v1.1 adds:
#   - Community flagging (Flag entry type, PollToFlags links)
#   - Migration support (MigratedPoll entry type, MigrationIndex links)
#
# Prerequisites:
#   - hc CLI 0.6.0: cargo install holochain_cli --version 0.6.0
#   - flowsta-agent-linking repo cloned at ../../flowsta-agent-linking/

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_LINKING_DIR="$SCRIPT_DIR/../../../flowsta-agent-linking"

echo "Building ProofPoll DNA v1.1"

# Check if Holochain CLI is installed
if ! command -v hc &> /dev/null; then
    echo "Error: Holochain CLI (hc) not found"
    echo "Install with: cargo install holochain_cli --version 0.6.0"
    exit 1
fi

# Create workdir if it doesn't exist
mkdir -p workdir

# Step 1: Build agent_linking zomes from the external crate
echo "Building agent_linking zomes from flowsta-agent-linking..."
if [ ! -d "$AGENT_LINKING_DIR" ]; then
    echo "Error: flowsta-agent-linking not found at $AGENT_LINKING_DIR"
    echo "Clone it from: https://github.com/WeAreFlowsta/flowsta-agent-linking"
    exit 1
fi
RUSTFLAGS='--cfg getrandom_backend="custom"' CARGO_TARGET_DIR="$AGENT_LINKING_DIR/target" \
    cargo build --release --target wasm32-unknown-unknown \
    --manifest-path "$AGENT_LINKING_DIR/Cargo.toml"

# Step 2: Build polls zomes (v1.1)
echo "Building polls zomes (v1.1)..."
RUSTFLAGS='--cfg getrandom_backend="custom"' CARGO_TARGET_DIR=target \
    cargo build --release --target wasm32-unknown-unknown

# Step 3: Copy all WASM files to workdir
echo "Copying WASM files..."
cp "$AGENT_LINKING_DIR/target/wasm32-unknown-unknown/release/flowsta_agent_linking_integrity.wasm" \
    workdir/agent_linking_integrity.wasm
cp "$AGENT_LINKING_DIR/target/wasm32-unknown-unknown/release/flowsta_agent_linking_coordinator.wasm" \
    workdir/agent_linking_coordinator.wasm
cp target/wasm32-unknown-unknown/release/polls_integrity.wasm workdir/
cp target/wasm32-unknown-unknown/release/polls_coordinator.wasm workdir/

# Step 4: Pack the DNA
echo "Packing DNA..."
hc dna pack workdir

# Step 5: Pack the hApp
echo "Packing hApp..."
hc app pack workdir

# Step 6: Copy hApp to Tauri resources
RESOURCES_DIR="$SCRIPT_DIR/../../src-tauri/resources"
if [ -d "$RESOURCES_DIR" ] || [ -d "$SCRIPT_DIR/../../src-tauri" ]; then
    mkdir -p "$RESOURCES_DIR"
    cp workdir/proofpoll_v1_1_happ.happ "$RESOURCES_DIR/"
    echo "Copied hApp to src-tauri/resources/"
fi

echo ""
echo "Build complete!"
echo ""
echo "Outputs:"
echo "  - DNA: workdir/proofpoll_v1_1.dna"
echo "  - hApp: workdir/proofpoll_v1_1_happ.happ"
