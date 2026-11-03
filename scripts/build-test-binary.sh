#!/usr/bin/env bash
# A ProofPoll test build: debug, the staging network and its auth, the test
# harness (FEATURES, default `harness`; the UI suite adds `e2e`). Network
# values are compile-time. The auth material and the client id are staging's
# test values (the same the Vault's test builds use), never production's.
# Usage: scripts/build-test-binary.sh [extra tauri build args]
set -euo pipefail
cd "$(dirname "$0")/.."
PROOFPOLL_BOOTSTRAP_URL=https://bootstrap-staging.flowsta.com \
PROOFPOLL_SIGNAL_URL=wss://bootstrap-staging.flowsta.com \
PROOFPOLL_RELAY_URL=https://bootstrap-staging.flowsta.com./ \
PROOFPOLL_BOOTSTRAP_URL_07=https://bootstrap-staging-7.flowsta.com \
PROOFPOLL_RELAY_URL_07=https://bootstrap-staging-7.flowsta.com./ \
PROOFPOLL_AUTH_MATERIAL=eyJjbGllbnRfaWQiOiJmbG93c3RhX2FwcF9iZGZmZDBjMTcwOTBiZDRkMzEyMWUwZjZkZGUzMmE0MDJjYWNmMTk3NmNiYjIzNDgzODA1MDAyZTJkNmE0Zjk0In0= \
VITE_FLOWSTA_CLIENT_ID=flowsta_app_2f0660aa9c7afdda85e8e8fc88e59cbcce79921cdecf9c81a2cbba8e34420a9b \
npx tauri build --debug --no-bundle --features "${FEATURES:-harness}" "$@"
