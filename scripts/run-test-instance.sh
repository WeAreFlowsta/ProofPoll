#!/usr/bin/env bash
# Start a ProofPoll test instance (a build from scripts/build-test-binary.sh):
# its own data folder, admin ports and harness port, and the one Vault it
# talks to. Every override is read by debug builds only.
# Usage: scripts/run-test-instance.sh <suffix> <port offset> <harness port> [vault port]
set -euo pipefail
SUFFIX="${1:?suffix}"; OFFSET="${2:?port offset, e.g. 100}"; HARNESS="${3:?harness port, e.g. 28801}"; VAULT="${4:-}"
DIR="$HOME/.local/share/proofpoll-test-$SUFFIX"
mkdir -p "$DIR"
cd "$(dirname "$0")/.."
export PROOFPOLL_DATA_DIR="$DIR"
# The webview's storage and the app log follow the XDG folders.
export XDG_DATA_HOME="$DIR/xdg" XDG_CACHE_HOME="$DIR/xdg-cache"
export PROOFPOLL_TEST_PORT_OFFSET="$OFFSET" PROOFPOLL_HARNESS_PORT="$HARNESS"
[ -n "$VAULT" ] && export PROOFPOLL_TEST_VAULT_PORT="$VAULT"
exec "${PROOFPOLL_TEST_BIN:-src-tauri/target/debug/proofpoll}"
