#!/usr/bin/env bash
# The Vault holds an identity ProofPoll was never connected under.
# ProofPoll is signed in under A; the Vault then holds B and names nobody to
# ProofPoll. A plain start stays on A's profile (the page says the Vault is
# signed in as someone else: UI spec 06a). "Open as this identity" (its
# restart marker) opens a new, empty profile beside A's and starts its
# sign-in, which ends connected under B. With the Vault on A
# again, ProofPoll opens A's profile, its link and link token as they were.
# Needs both test builds (scripts/build-test-binary.sh here and in the Vault repo).
# Usage: scripts/another-identity.sh [log dir]
set -u
PP="$(cd "$(dirname "$0")/.." && pwd)"; V="${VAULT_REPO:-$PP/../flowsta-vault}"
G="${1:-$HOME/.cache/proofpoll-matrix/another-$(date -u +%m%d-%H%M)}"; mkdir -p "$G"; FAILED=0
check() { if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1 — expected $3, got $2"; FAILED=1; fi; }
PROF=$HOME/.local/share/proofpoll-test-pp-a
nap() { python3 -c "import time; time.sleep($1)"; }
killdebug() { for n in proofpoll flowsta-vault; do for pid in $(pgrep -x $n); do case "$(readlink /proc/$pid/exe 2>/dev/null)" in *target/debug*) kill "$pid" 2>/dev/null;; esac; done; done; nap 8; }
vport() { for p in 27777 27778 27779; do curl -s -m 2 "http://127.0.0.1:$p/dev/status" | grep -q '"harness":true' && { echo $p; return; }; done; }
startvault() { (cd "$V" && exec dbus-run-session -- scripts/run-test-instance.sh pp-vault) >> "$G/vault.log" 2>&1 & for _ in $(seq 1 40); do [ -n "$(vport)" ] && break; nap 3; done; }
startpp() { (cd "$PP" && dbus-run-session -- scripts/run-test-instance.sh pp-a 100 28801 "$1" > "$G/pp-$2.log" 2>&1 &); for _ in $(seq 1 90); do curl -s -m 2 http://127.0.0.1:28801/dev/status | grep -q '"ready":true' && break; nap 2; done; nap 30; }
stoppp() { for pid in $(pgrep -x proofpoll); do case "$(readlink /proc/$pid/exe)" in *target/debug*) kill "$pid";; esac; done; nap 10; }
token() { [ -f "$PROF/profiles/local/vault-link-token.json" ] && echo present || echo absent; }
cd "$PP"; killdebug
rm -rf "$HOME/.local/share/flowsta-vault-pp-vault" "$PROF"
startvault; VP=$(vport); echo "vault $VP"
PW="Repro-$(openssl rand -hex 6)!"
A=$(curl -s -m 300 -X POST "http://127.0.0.1:$VP/dev/setup-identity" -H 'content-type: application/json' -H "origin: https://ourtest.flowsta.com" -d "{\"api_url\":\"https://auth-api-staging.flowsta.com\",\"password\":\"$PW\",\"email\":\"pp-repro-a-$(date +%s)@example.com\",\"display_name\":\"Identity A\"}" | python3 -c "import sys,json; print(json.load(sys.stdin).get('agent_pub_key'))")
echo "A $A"; nap 20
export PP_A=http://127.0.0.1:28801 VAULT="http://127.0.0.1:$VP" CLIENT_ID="$(grep -o 'VITE_FLOWSTA_CLIENT_ID=[^ \\]*' scripts/build-test-binary.sh | cut -d= -f2-)"
startpp "$VP" 1
node scripts/pp-matrix.mjs --leg=fresh > "$G/leg-fresh.log" 2>&1; tail -1 "$G/leg-fresh.log"
node scripts/pp-matrix.mjs --leg=signin > "$G/leg-signin.log" 2>&1; tail -1 "$G/leg-signin.log"
check "after sign-in the profile holds a link token" "$(token)" present
stoppp; startpp "$VP" 2
check "a restart with the Vault on A opens A's profile" "$(grep -h 'Profile root' "$G/pp-2.log" | sed 's/.*profiles\///; s/"//')" local
check "and the Vault refused nothing" "$(grep -c 'refused this profile' "$G/pp-2.log")" 0
stoppp
curl -s -m 30 -X POST "http://127.0.0.1:$VP/dev/lock" >/dev/null; nap 5
KEYA=$(curl -s -m 10 "http://127.0.0.1:$VP/dev/identities" -H "origin: https://ourtest.flowsta.com" | python3 -c "import sys,json; print([i['key'] for i in json.load(sys.stdin)['identities'] if i.get('active')][0])")
B=$(curl -s -m 300 -X POST "http://127.0.0.1:$VP/dev/setup-identity" -H 'content-type: application/json' -H "origin: https://ourtest.flowsta.com" -d "{\"api_url\":\"https://auth-api-staging.flowsta.com\",\"password\":\"$PW\",\"email\":\"pp-another-b-$(date +%s)@example.com\",\"display_name\":\"Identity B\"}" | python3 -c "import sys,json; print(json.load(sys.stdin).get('agent_pub_key'))")
echo "B $B"; nap 20
check "the Vault, on B, names nobody to the app's origin" "$(curl -s -m 5 -H 'origin: tauri://localhost' http://127.0.0.1:$VP/status | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('unlocked'), d.get('active_identity'))")" "True None"
startpp "$VP" 3
check "a plain start with the Vault on B stays on A's profile" "$(grep -h 'Profile root' "$G/pp-3.log" | sed 's/.*profiles\///; s/"//')" local
check "A's link token is kept" "$(token)" present
stoppp
# What "Open as this identity" leaves for the next start.
: > "$PROF/relaunch-into-identity"
startpp "$VP" 4
check "asked to open as the Vault's identity, ProofPoll opens a new profile" "$(grep -h 'Profile root' "$G/pp-4.log" | sed 's/.*profiles\///; s/"//')" local-2
# The new profile starts its sign-in by itself; the person approves it in the
# Vault's dialog (the test Vault approves by itself).
for _ in $(seq 1 60); do curl -s -m 5 http://127.0.0.1:28801/dev/status | grep -q '"vault_agent_pub_key"' && break; nap 3; done
check "which signs in as the identity the Vault holds" "$(curl -s -m 5 http://127.0.0.1:28801/dev/status | python3 -c "import sys,json; print((json.load(sys.stdin).get('identity_link') or {}).get('vault_agent_pub_key'))")" "$B"
check "A's profile still holds its link" "$([ -f "$PROF/profiles/local/identity-link.json" ] && echo yes || echo no)" yes
check "and its link token" "$(token)" present
stoppp; startpp "$VP" 5
check "the next plain start stays on the new profile" "$(grep -h 'Profile root' "$G/pp-5.log" | sed 's/.*profiles\///; s/"//')" local-2
stoppp
curl -s -m 30 -X POST "http://127.0.0.1:$VP/dev/lock" >/dev/null; nap 3
curl -s -m 30 -X POST "http://127.0.0.1:$VP/dev/select-identity" -H 'content-type: application/json' -H "origin: https://ourtest.flowsta.com" -d "{\"key\":\"$KEYA\"}" >/dev/null
curl -s -m 120 -X POST "http://127.0.0.1:$VP/dev/unlock-with-password" -H 'content-type: application/json' -H "origin: https://ourtest.flowsta.com" -d "{\"password\":\"$PW\"}" >/dev/null; nap 20
startpp "$VP" 6
check "with the Vault on A again, ProofPoll opens A's profile" "$(grep -h 'Profile root' "$G/pp-6.log" | sed 's/.*profiles\///; s/"//')" local
check "signed in as A" "$(curl -s -m 5 http://127.0.0.1:28801/dev/status | python3 -c "import sys,json; print((json.load(sys.stdin).get('identity_link') or {}).get('vault_agent_pub_key'))")" "$A"
check "and the Vault refused nothing" "$(grep -c 'refused this profile' "$G/pp-6.log")" 0
killdebug
[ "$FAILED" = 0 ] && echo "RESULT: ALL GREEN" || echo "RESULT: FAILED"
exit $FAILED
