#!/bin/bash

# Build script for ProofPoll DNA v1.5
#
# v1.5 is v1.4 with carried records the network checks (a new DNA hash, so
# a new network on the same Holochain 0.7 line):
#   - Poll, Vote and Flag can carry their original record from v1.3 or v1.4,
#     and integrity checks it against the original author's signature
#   - CarriedLink: an agent link carried the same way
#
# The app bundles this pack as src-tauri/resources/proofpoll_v1_5_happ.happ
# (copied by hand, its sha256 in src-tauri/src/kits.rs). Once a release has
# shipped it, never rebuild it: a new build is a new DNA hash, a new network.
# The sweettests in tests/ load it from workdir/.
#
# Prerequisites:
#   - hc CLI 0.7.1 (cargo install holochain_cli --version 0.7.1), reachable
#     as `hc-0.7` or named in $HC
#   - Rust 1.94 or newer with the wasm32 target (rust-toolchain.toml pins it)
#   - flowsta-agent-linking on its holochain-0.7 line, cloned at
#     ../../../flowsta-agent-linking/

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_LINKING_DIR="$SCRIPT_DIR/../../../flowsta-agent-linking"

echo "Building ProofPoll DNA v1.5"

if ! command -v ${HC:-hc-0.7} &> /dev/null; then
    echo "Error: Holochain CLI (hc) not found"
    echo "Install with: cargo install holochain_cli --version 0.7.1"
    exit 1
fi

mkdir -p workdir

echo "Building agent_linking zomes from flowsta-agent-linking..."
if [ ! -d "$AGENT_LINKING_DIR" ]; then
    echo "Error: flowsta-agent-linking not found at $AGENT_LINKING_DIR"
    echo "Clone it from: https://github.com/WeAreFlowsta/flowsta-agent-linking"
    exit 1
fi
RUSTFLAGS='--cfg getrandom_backend="custom"' CARGO_TARGET_DIR="$AGENT_LINKING_DIR/target" \
    cargo build --release --target wasm32-unknown-unknown \
    --manifest-path "$AGENT_LINKING_DIR/Cargo.toml"

echo "Building polls zomes (v1.5)..."
RUSTFLAGS='--cfg getrandom_backend="custom"' CARGO_TARGET_DIR=target \
    cargo build --release --target wasm32-unknown-unknown

echo "Copying WASM files..."
cp "$AGENT_LINKING_DIR/target/wasm32-unknown-unknown/release/flowsta_agent_linking_integrity.wasm" \
    workdir/agent_linking_integrity.wasm
cp "$AGENT_LINKING_DIR/target/wasm32-unknown-unknown/release/flowsta_agent_linking_coordinator.wasm" \
    workdir/agent_linking_coordinator.wasm
cp target/wasm32-unknown-unknown/release/polls_integrity.wasm workdir/
cp target/wasm32-unknown-unknown/release/polls_coordinator.wasm workdir/

echo "Packing DNA..."
${HC:-hc-0.7} dna pack workdir

echo "Packing hApp..."
${HC:-hc-0.7} app pack workdir


echo ""
echo "Build complete!"
echo ""
echo "Outputs:"
echo "  - DNA: workdir/proofpoll_v1_5.dna"
echo "  - hApp: workdir/proofpoll_v1_5_happ.happ"
