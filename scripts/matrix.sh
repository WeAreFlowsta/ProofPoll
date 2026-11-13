#!/usr/bin/env bash
# ProofPoll's matrix end to end: a Flowsta Vault test instance with a staging
# identity (the Vault repo beside this one, its test binary built), ProofPoll
# test instances A and B (scripts/build-test-binary.sh), the legs in order;
# C and D join for the restore (its quorum: two peers agreeing on the chain head).
# Usage: scripts/matrix.sh [log dir]     (LEGS="fresh signin polls migration devices offline adopt short")
# `short` (a restore that comes back shorter than the backup, then whole) runs
# after `adopt`; LEGS="... adopt short-go-on" runs its other ending instead.
# The node's carrier on its own: LEGS=carrier scripts/matrix.sh (every instance
# holds its own move, and carrier/target/debug/proofpoll-carrier is built).
# Runs beside an installed ProofPoll and Vault: its own folders and ports.
set -u
cd "$(dirname "$0")/.."
PP="$PWD"
VAULT_REPO="${VAULT_REPO:-$PP/../flowsta-vault}"
LOGS="${1:-$HOME/.cache/proofpoll-matrix/run-$(date -u +%m%d-%H%M)}"; mkdir -p "$LOGS"
LEGS="${LEGS:-fresh signin polls migration devices offline adopt short}"
ORIGIN=https://ourtest.flowsta.com
CLIENT_ID="$(grep -o 'VITE_FLOWSTA_CLIENT_ID=[^ \\]*' scripts/build-test-binary.sh | cut -d= -f2-)"
rc=0

stop_all() {
  for pid in $(pgrep -x proofpoll); do case "$(readlink /proc/$pid/exe 2>/dev/null)" in "$PP"/src-tauri/target/debug/proofpoll) kill -TERM $pid;; esac; done
  for pid in $(pgrep -x flowsta-vault); do case "$(readlink /proc/$pid/exe 2>/dev/null)" in */flowsta-vault/src-tauri/target/debug/flowsta-vault) kill -TERM $pid;; esac; done
  sleep 8
  for d in pp-a pp-b pp-c pp-d; do for pid in $(pgrep -f "proofpoll-test-$d/"); do [ "$pid" != "$$" ] && kill -9 $pid 2>/dev/null; done; done
  for pid in $(pgrep -f "flowsta-vault-pp-vault/"); do [ "$pid" != "$$" ] && kill -9 $pid 2>/dev/null; done
}
trap stop_all EXIT
# Stop the test instance whose harness listens on a port, and wait for it.
stop_instance() {
  local pid; pid=$(ss -ltnp 2>/dev/null | awk -v p=":$1\$" '$4 ~ p' | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
  [ -n "$pid" ] && kill -TERM "$pid"
  for i in $(seq 1 30); do pgrep -f "proofpoll-test-$2/" >/dev/null || break; sleep 1; done
}
# Wait until a test instance's harness answers (its window is up).
wait_harness() { for i in $(seq 1 "${2:-60}"); do curl -s -m 2 "http://127.0.0.1:$1/dev/status" | grep -q '"harness":true' && return 0; sleep 2; done; echo "  (the harness on $1 never answered)"; return 1; }

# 1. The Vault test instance, with a staging identity. Whatever an earlier run
# (this matrix, the UI suite) left running goes first, or its Vault answers.
stop_all
rm -rf "$HOME/.local/share/flowsta-vault-pp-vault" "$HOME/.local/share/proofpoll-test-pp-a" "$HOME/.local/share/proofpoll-test-pp-b" "$HOME/.local/share/proofpoll-test-pp-c" "$HOME/.local/share/proofpoll-test-pp-d"
(cd "$VAULT_REPO" && exec dbus-run-session -- scripts/run-test-instance.sh pp-vault) > "$LOGS/vault.log" 2>&1 &
VPORT=""
for i in $(seq 1 40); do
  for p in 27777 27778 27779; do curl -s -m 2 "http://127.0.0.1:$p/dev/status" | grep -q '"harness":true' && { VPORT=$p; break; }; done
  [ -n "$VPORT" ] && break; sleep 3
done
[ -n "$VPORT" ] || { echo "the Vault test instance never answered"; exit 1; }
PW="Matrix-$(openssl rand -hex 6)!"
curl -s -m 300 -X POST "http://127.0.0.1:$VPORT/dev/setup-identity" -H 'content-type: application/json' -H "origin: $ORIGIN" \
  -d "{\"api_url\":\"https://auth-api-staging.flowsta.com\",\"password\":\"$PW\",\"email\":\"pp-matrix-$(date +%s)@example.com\",\"display_name\":\"ProofPoll Matrix\"}" > "$LOGS/vault-identity.json"
echo "== Vault test instance on $VPORT: $(python3 -c 'import sys,json; d=json.load(open(sys.argv[1])); print(d.get("did") or d)' "$LOGS/vault-identity.json" 2>/dev/null | cut -c1-80)"

# Instances that stand for installs that have not updated: no move of their own.
[ "$LEGS" = carrier ] && export PROOFPOLL_TEST_HOLD_MOVE=1

# 2. ProofPoll A.
dbus-run-session -- scripts/run-test-instance.sh pp-a 100 28801 "$VPORT" > "$LOGS/pp-a.log" 2>&1 &
export PP_A=http://127.0.0.1:28801 PP_B=http://127.0.0.1:28802 PP_C=http://127.0.0.1:28803 PP_D=http://127.0.0.1:28804 VAULT="http://127.0.0.1:$VPORT" CLIENT_ID
STATE="$LOGS/polls-state.json"
for leg in $LEGS; do
  echo "== leg $leg $(date -u +%H:%M:%S)"
  if [ "$leg" = devices ]; then
    dbus-run-session -- scripts/run-test-instance.sh pp-b 110 28802 "$VPORT" > "$LOGS/pp-b.log" 2>&1 &
    PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" node scripts/pp-matrix.mjs --leg=devices > "$LOGS/leg-$leg.log" 2>&1 || rc=1
  elif [ "$leg" = offline ]; then
    # A again, its rendezvous checks failing for 90 s (a debug switch).
    APID=$(ss -ltnp 2>/dev/null | awk '$4 ~ /:28801$/' | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
    [ -n "$APID" ] && kill -TERM "$APID"
    for i in $(seq 1 20); do pgrep -f "proofpoll-test-pp-a/" >/dev/null || break; sleep 1; done
    PROOFPOLL_TEST_OFFLINE_UNTIL=$(( $(date +%s) + 90 )) dbus-run-session -- scripts/run-test-instance.sh pp-a 100 28801 "$VPORT" >> "$LOGS/pp-a.log" 2>&1 &
    PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" node scripts/pp-matrix.mjs --leg=offline > "$LOGS/leg-$leg.log" 2>&1 || rc=1
  elif [ "$leg" = adopt ]; then
    # C and D join as peers that stay up (a restore needs two agreeing on
    # A's chain head); A backs up, B adopts its seed and exits; A stops (the
    # dead machine), B starts again as A's agent and restores A's chain.
    # One after the other: two windows starting in the same second collided
    # on their sessions' accessibility bus (11-04).
    dbus-run-session -- scripts/run-test-instance.sh pp-c 120 28803 "$VPORT" > "$LOGS/pp-c.log" 2>&1 &
    wait_harness 28803 || true
    dbus-run-session -- scripts/run-test-instance.sh pp-d 130 28804 "$VPORT" > "$LOGS/pp-d.log" 2>&1 &
    wait_harness 28804 || true
    PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" PP_STATE_OUT="$STATE" node scripts/pp-matrix.mjs --leg=adopt > "$LOGS/leg-$leg.log" 2>&1 || rc=1
    APID=$(ss -ltnp 2>/dev/null | awk '$4 ~ /:28801$/' | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
    [ -n "$APID" ] && kill -TERM "$APID"
    for i in $(seq 1 60); do ss -ltn | grep -qE ':(28801|28802)\b' || break; sleep 1; done
    for i in $(seq 1 20); do pgrep -f "proofpoll-test-pp-(a|b)/" >/dev/null || break; sleep 1; done
    dbus-run-session -- scripts/run-test-instance.sh pp-b 110 28802 "$VPORT" >> "$LOGS/pp-b.log" 2>&1 &
    PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" node scripts/pp-matrix.mjs --leg=adopted >> "$LOGS/leg-$leg.log" 2>&1 || rc=1
  elif [ "$leg" = short ] || [ "$leg" = short-go-on ]; then
    # After `adopt` (B holds the key; C and D are up). B starts again with
    # no network for good, writes and backs up; A's folder is a new device
    # that adopts the seed, and its restore comes back shorter than the
    # backup. `short`: B comes back online and A opens whole.
    # `short-go-on`: B stays gone and the person goes on. One or the other
    # in a run: a device that went on must never meet the old one again.
    stop_instance 28802 pp-b; stop_instance 28801 pp-a
    PROOFPOLL_TEST_OFFLINE_UNTIL=$(( $(date +%s) + 86400 )) dbus-run-session -- scripts/run-test-instance.sh pp-b 110 28802 "$VPORT" >> "$LOGS/pp-b.log" 2>&1 &
    PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" PP_STATE_OUT="$STATE" node scripts/pp-matrix.mjs --leg=short-write > "$LOGS/leg-$leg.log" 2>&1 || rc=1
    rm -rf "$HOME/.local/share/proofpoll-test-pp-a"
    dbus-run-session -- scripts/run-test-instance.sh pp-a 100 28801 "$VPORT" >> "$LOGS/pp-a.log" 2>&1 &
    node scripts/pp-matrix.mjs --leg=short-adopt >> "$LOGS/leg-$leg.log" 2>&1 || rc=1
    for i in $(seq 1 60); do ss -ltn | grep -qE ':28801\b' || break; sleep 1; done
    for i in $(seq 1 20); do pgrep -f "proofpoll-test-pp-a/" >/dev/null || break; sleep 1; done
    dbus-run-session -- scripts/run-test-instance.sh pp-a 100 28801 "$VPORT" >> "$LOGS/pp-a.log" 2>&1 &
    PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" node scripts/pp-matrix.mjs --leg=short-waits >> "$LOGS/leg-$leg.log" 2>&1 || rc=1
    stop_instance 28802 pp-b
    if [ "$leg" = short ]; then
      dbus-run-session -- scripts/run-test-instance.sh pp-b 110 28802 "$VPORT" >> "$LOGS/pp-b.log" 2>&1 &
      PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" node scripts/pp-matrix.mjs --leg=short-whole >> "$LOGS/leg-$leg.log" 2>&1 || rc=1
      stop_instance 28802 pp-b
    else
      PP_STATE="$(cat "$STATE" 2>/dev/null || echo '{}')" node scripts/pp-matrix.mjs --leg=short-go-on >> "$LOGS/leg-$leg.log" 2>&1 || rc=1
    fi
  elif [ "$leg" = carrier ]; then
    # B and C vote, D stands for the node; one after the other (their
    # sessions' accessibility bus).
    for x in "pp-b 110 28802" "pp-c 120 28803" "pp-d 130 28804"; do
      read -r name offset port <<< "$x"
      dbus-run-session -- scripts/run-test-instance.sh "$name" "$offset" "$port" "$VPORT" > "$LOGS/$name.log" 2>&1 &
      wait_harness "$port" || true
    done
    node scripts/pp-matrix.mjs --leg=carrier > "$LOGS/leg-$leg.log" 2>&1 || rc=1
  else
    PP_STATE_OUT="$STATE" node scripts/pp-matrix.mjs --leg="$leg" > "$LOGS/leg-$leg.log" 2>&1 || rc=1
  fi
  grep -E "✓|✗|RESULT" "$LOGS/leg-$leg.log" | sed 's/^/  /'
done

stop_all; trap - EXIT
for d in pp-a pp-b pp-c pp-d; do
  f="$HOME/.local/share/proofpoll-test-$d/xdg/com.proofpoll.app/logs/proofpoll.log"; [ -f "$f" ] && cp "$f" "$LOGS/$d-app.log"
  for c in $(find "$HOME/.local/share/proofpoll-test-$d" -name 'holochain-stderr.log' 2>/dev/null); do cp "$c" "$LOGS/$d-$(basename "$(dirname "$c")")-stderr.log"; done
done
echo "== logs in $LOGS"
exit $rc
