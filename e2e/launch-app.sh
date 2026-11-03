#!/usr/bin/env bash
# Launches the e2e build of ProofPoll on an ISOLATED data folder for the UI
# tests (see e2e/README.md). @wdio/tauri-service runs this instead of the
# binary, so the app never sees the person's own ProofPoll: its data lives in
# e2e/profile (wiped per run), its conductors on their own ports, it talks to
# STAGING and to the Vault test instance wdio.conf.ts started
# (PROOFPOLL_E2E_VAULT_PORT), never to an installed Vault.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export PROOFPOLL_DATA_DIR="${PROOFPOLL_E2E_DATA_DIR:-$HERE/profile/a}"
mkdir -p "$PROOFPOLL_DATA_DIR"
# The webview's storage and the app's log follow the XDG folders.
export XDG_DATA_HOME="$PROOFPOLL_DATA_DIR/xdg" XDG_CACHE_HOME="$PROOFPOLL_DATA_DIR/xdg-cache"
mkdir -p "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
export PROOFPOLL_TEST_PORT_OFFSET="${PROOFPOLL_E2E_PORT_OFFSET:-200}"
export PROOFPOLL_HARNESS_PORT="${PROOFPOLL_E2E_HARNESS_PORT:-28811}"
VPORT="${PROOFPOLL_E2E_VAULT_PORT:-$(cat "$HERE/profile/vault-port" 2>/dev/null || true)}"
[ -n "$VPORT" ] && export PROOFPOLL_TEST_VAULT_PORT="$VPORT"
# On a Wayland desktop the window is an XWayland window, which xdotool can see.
if [ "${XDG_SESSION_TYPE:-}" = "wayland" ] && [ -z "${PROOFPOLL_E2E_NATIVE_WAYLAND:-}" ]; then export GDK_BACKEND=x11; fi
APP="$(cd "$HERE/../src-tauri/target/debug" && pwd)/proofpoll"
exec dbus-run-session -- "$APP" "$@"
