# ProofPoll

Verified polls on Holochain with Flowsta identity linking.

ProofPoll is a desktop app (Tauri v2 + Qwik) that runs a local Holochain conductor. Polls and votes are stored on a decentralized DHT — no central server. Identity verification through Flowsta ensures one vote per real person.

**This app is designed to be forked.** Change the entry types, swap polls for reviews or proposals, add your own features — the architecture (conductor management, identity linking, DNA migration) works for any Holochain app. See [Forking Guide](#forking-guide) below.

## Stack

- **Frontend**: Qwik, TypeScript, Tailwind CSS
- **Backend**: Tauri v2 (Rust), Holochain 0.6.0
- **DNA**: Rust (hdi 0.7.0, hdk 0.6.0)
- **Identity**: Flowsta agent linking via `flowsta-agent-linking` crate

## Quick Start

```bash
# Prerequisites
# - Rust + wasm32-unknown-unknown target
# - holochain + lair-keystore binaries (v0.6.0)
# - hc CLI: cargo install holochain_cli --version 0.6.0
# - Node.js 18+
# - flowsta-agent-linking repo cloned at ../flowsta-agent-linking/

# Build both DNA versions
bash build-all.sh

# Install frontend dependencies
npm install

# Run in dev mode
cargo tauri dev
```

## DNA Versions

| Version | Network Seed | Features |
|---|---|---|
| v1.0 | `proofpoll-network-v1.0` | Polls, votes, agent linking |
| v1.1 | `proofpoll-network-v1.1` | + Community flagging, migration support |

Both versions are installed side-by-side during migration. All new reads and writes go to v1.1.

## Community Flagging (v1.1)

Polls can be flagged by signed-in users for: Spam, Misleading, Off Topic, or Inappropriate.

- **Censorship-resistant**: Flags are a UI-layer opinion. The poll and all votes remain on the DHT forever. No data is deleted.
- **Sybil-resistant**: One flag per Flowsta identity per poll (same deduplication as votes).
- **Configurable threshold**: Polls with >= `FLAG_HIDE_THRESHOLD` flags (default 3) are hidden from the default view. Users can toggle "Show flagged" to see them.
- **Forking developers**: Change `FLAG_HIDE_THRESHOLD` in `dna/v1.1/zomes/polls/coordinator/src/lib.rs` to suit your community size.

---

## Forking Guide

This section is for developers (and AIs) who want to fork ProofPoll into a completely different app — a review platform, a task tracker, a social feed, anything. The architecture is app-agnostic; the poll/vote specifics are easy to swap out.

### Step 1: Rename Everything

These identifiers **must** change or your app will conflict with ProofPoll:

| What | Where | Current Value | Change To |
|---|---|---|---|
| Bundle ID | `src-tauri/tauri.conf.json` line 5 | `com.proofpoll.app` | `com.yourcompany.yourapp` |
| Product name | `src-tauri/tauri.conf.json` line 3 | `ProofPoll` | `YourApp` |
| Window title | `src-tauri/tauri.conf.json` line 15 | `ProofPoll` | `YourApp` |
| Rust crate name | `src-tauri/Cargo.toml` line 2 | `proofpoll` | `yourapp` |
| Rust description | `src-tauri/Cargo.toml` line 4 | `ProofPoll - ...` | Your description |
| npm package name | `package.json` line 2 | `proofpoll` | `yourapp` |
| DNA name (v1.0) | `dna/workdir/dna.yaml` line 3 | `proofpoll_v1_0` | `yourapp_v1_0` |
| DNA name (v1.1) | `dna/v1.1/workdir/dna.yaml` line 3 | `proofpoll_v1_1` | `yourapp_v1_1` |
| Network seed (v1.0) | `dna/workdir/dna.yaml` line 5 | `proofpoll-network-v1.0` | `yourapp-network-v1.0` |
| Network seed (v1.1) | `dna/v1.1/workdir/dna.yaml` line 5 | `proofpoll-network-v1.1` | `yourapp-network-v1.1` |
| hApp name (v1.0) | `dna/workdir/happ.yaml` line 3 | `proofpoll_v1_0_happ` | `yourapp_v1_0_happ` |
| hApp name (v1.1) | `dna/v1.1/workdir/happ.yaml` line 3 | `yourapp_v1_1_happ` | `yourapp_v1_1_happ` |
| hApp role (v1.0) | `dna/workdir/happ.yaml` lines 6-7 | `proofpoll` | `yourapp` |
| hApp role (v1.1) | `dna/v1.1/workdir/happ.yaml` lines 6-7 | `proofpoll` | `yourapp` |

Then update these Rust constants to match:

| File | Constant | Current | Change To |
|---|---|---|---|
| `src-tauri/src/dna.rs` | `APP_ID_V1_0` | `"proofpoll_v1_0"` | `"yourapp_v1_0"` |
| `src-tauri/src/dna.rs` | `APP_ID_V1_1` | `"proofpoll_v1_1"` | `"yourapp_v1_1"` |
| `src-tauri/src/dna.rs` | `HAPP_FILE_V1_0` | `"proofpoll_v1_0_happ.happ"` | `"yourapp_v1_0_happ.happ"` |
| `src-tauri/src/dna.rs` | `HAPP_FILE_V1_1` | `"proofpoll_v1_1_happ.happ"` | `"yourapp_v1_1_happ.happ"` |
| `src-tauri/src/commands.rs` | `ROLE_NAME` | `"proofpoll"` | `"yourapp"` |
| `src-tauri/src/migration.rs` | `RoleName::from(...)` | `"proofpoll"` | `"yourapp"` |

Also update the `"proofpoll"` string passed to `AdminWebsocket::connect(...)` and `AppWebsocket::connect(...)` calls throughout `dna.rs` (appears 6 times as the origin parameter).

Update build scripts (`dna/build.sh`, `dna/v1.1/build.sh`, `build-all.sh`) — change hApp filenames in the `cp` commands and echo messages.

**Critical**: The `network_seed` in `dna.yaml` determines which DHT your app joins. Two apps with the same network seed share a DHT. Always use a unique seed.

### Step 2: Replace Entry Types

ProofPoll's data model is polls and votes. Replace these with your own.

**Integrity zome** (`dna/v1.1/zomes/polls/integrity/src/lib.rs`):

```rust
// REPLACE these with your entry types:
pub struct Poll { ... }     // → pub struct Review { ... }
pub struct Vote { ... }     // → pub struct Rating { ... }
pub struct Flag { ... }     // Keep or adapt for your content type

// RENAME these:
pub enum EntryTypes {
    Poll(Poll),             // → Review(Review)
    Vote(Vote),             // → Rating(Rating)
    Flag(Flag),             // Keep
    MigratedPoll(MigratedPoll), // → MigratedReview(MigratedReview)
}

pub enum LinkTypes {
    AllPolls,               // → AllReviews
    PollToVotes,            // → ReviewToRatings
    PollToFlags,            // → ReviewToFlags
    MigrationIndex,         // Keep as-is
}

// UPDATE the anchor function:
pub fn all_polls_anchor()   // → pub fn all_reviews_anchor()
```

**Coordinator zome** (`dna/v1.1/zomes/polls/coordinator/src/lib.rs`):

Replace the zome functions to match your data model. The patterns are reusable:
- `create_poll` → `create_review` (same anchor + link pattern)
- `cast_vote` → `submit_rating` (same double-action prevention pattern)
- `get_all_polls` → `get_all_reviews` (same anchor query pattern)
- `flag_poll` → `flag_review` (same pattern, just rename)

Keep the migration functions (`register_migrated_poll`, `get_migration_mapping`, `get_all_migration_mappings`) — rename "poll" to your content type but the logic is identical.

**Rename the zome package**: If you rename from "polls" to something else (e.g., "reviews"), update:
- Directory names: `dna/v1.1/zomes/polls/` → `dna/v1.1/zomes/reviews/`
- `dna/v1.1/Cargo.toml` workspace members
- `dna/v1.1/workdir/dna.yaml` zome names and paths
- `src-tauri/src/commands.rs`: `POLLS_ZOME` constant

### Step 3: Update Tauri Commands

`src-tauri/src/commands.rs` has mirror types and Tauri commands for each zome function.

**Replace** the poll/vote/flag Rust structs and commands with your own. The pattern is always:
1. Define a response struct (serializable)
2. `#[tauri::command]` function that locks the AppWebsocket, encodes payload, calls `call_zome`, decodes result

**Keep** these as-is (they're infrastructure):
- `AppState` struct and `new()`
- `call_zome()` helper (line 134)
- `try_reenable_app()` (line 182)
- `friendly_error()` (line 202)
- `decode_entry()` (line 131)
- `parse_agent_pub_key_string()` (line 225)
- `get_app_status` command
- `get_export_data` command (adapt the data it exports)
- Identity link commands (`commit_identity_link`, `get_identity_link`, etc.)
- Migration status commands (`get_migration_status`, `abandon_pending_votes`)

**Register new commands** in `src-tauri/src/lib.rs` → `invoke_handler(tauri::generate_handler![...])`.

### Step 4: Update Frontend

**`src/lib/holochain.ts`** — Replace poll/vote/flag TypeScript types and `invoke()` wrappers with your own. Keep the identity and migration functions.

**`src/routes/`** — Replace the pages:
- `index.tsx` → Your content list page
- `create/index.tsx` → Your content creation form
- `poll/[id]/index.tsx` → Your content detail page

**Keep as-is:**
- `layout.tsx` — Conductor startup, header, migration banner (just rename "ProofPoll" to your app name in the UI text)
- `identity/index.tsx` — Flowsta identity linking page (if using Flowsta)
- `src/lib/context.ts` — Qwik signals for linked state
- `src/lib/sanitize.ts` — XSS prevention

### Step 5: Update Migration

`src-tauri/src/migration.rs` exports polls and votes from v1.0 and re-creates them on v1.1. Replace the entry types and zome function names with your own. The orchestration pattern (export → create → register mapping → cast → retry loop) is identical for any data model.

The key structs to replace:
- `Poll`, `Vote` (lines ~100-120) → Your entry types
- `CreatePollInput`, `CastVoteInput` (lines ~130-145) → Your input types
- Zome function names in `call_zome_on()` calls: `"get_all_polls"`, `"create_poll"`, `"cast_vote"`, etc.

---

## Flowsta Integration Points

ProofPoll uses [Flowsta](https://flowsta.com) for decentralized identity verification. If you want to use Flowsta in your fork, keep these as-is and just change the client_id. If you want a different identity system (or none), remove them.

### Setup

1. Register your app at [dev.flowsta.com](https://dev.flowsta.com) to get a `client_id`
2. Update `.env`: `VITE_FLOWSTA_CLIENT_ID=your_client_id_here`
3. Clone `flowsta-agent-linking` at `../flowsta-agent-linking/` (referenced by build scripts)
4. Keep the `agent_linking_integrity` and `agent_linking` zomes in your `dna.yaml`
5. Update the `appName` parameter in `linkFlowstaIdentity()` calls to your app name (shown in the Vault approval dialog)

### How Identity Works at Runtime

The Flowsta Vault is a separate desktop app that manages the user's identity. Your app communicates with it via HTTP on `localhost:27777`. The key design principle: **the Vault only needs to be running for the initial identity linking ceremony**. After that, your app works independently.

**First launch (new user)**:
1. User clicks "Sign in with Flowsta" → calls `linkFlowstaIdentity()` from `@flowsta/holochain` SDK
2. The SDK sends `POST /link-identity` to the Vault → Vault shows approval dialog
3. Vault signs an attestation with the user's key → returned to your app
4. Your app commits the `IsSamePersonEntry` on the DHT via the `agent_linking` zome
5. Identity link data is saved locally (`identity-link.json`)
6. Display name and profile picture are fetched from Vault and cached locally (`profile-cache.json`)

**Subsequent launches (Vault running or not)**:
1. App loads `identity-link.json` → knows user was previously linked
2. App loads `profile-cache.json` → shows display name and avatar immediately
3. If Vault is running: refreshes profile and updates cache
4. If Vault is closed/locked: cached data is used — everything works normally
5. DHT entry is re-created in the background when Vault is available (for peer verification)

**Key files for this flow**:
- `src/routes/layout.tsx` — Startup link detection, profile cache loading, Vault polling
- `src-tauri/src/commands.rs` — `get_identity_link`, `get_cached_profile`, `save_profile_cache` commands
- `src/lib/holochain.ts` — TypeScript wrappers for all identity + profile functions

### Constants reference

| Value | Location | Purpose |
|---|---|---|
| `VITE_FLOWSTA_CLIENT_ID` | `.env` | Identifies your app to Flowsta |
| `http://127.0.0.1:27777` | `layout.tsx`, `identity/index.tsx`, `commands.rs` | Flowsta Vault IPC server |
| `@flowsta/holochain` | `package.json`, `layout.tsx`, `identity/index.tsx` | Flowsta SDK for identity linking |
| `flowsta-agent-linking` | `build.sh`, `dna.yaml` | Reusable Rust crate for DHT identity attestations |
| `"ProofPoll"` in `linkFlowstaIdentity()` | `layout.tsx`, `identity/index.tsx` | App name shown in Vault approval dialog |
| Port `5174` | `vite.config.ts` | Dev port (avoids conflict with Vault on 5173) |
| Port `4466` | `conductor.rs` | Admin WS port (avoids conflict with Vault on 4455) |

---

## DNA Migration

This app includes a complete migration system for upgrading between DNA versions. This section explains how it works and how to add your own versions.

### The Problem

Holochain DNA versions with different integrity zomes (new entry types, changed validation) get different DNA hashes, which means a **new DHT**. Old data lives on the old DHT. Each user runs their own conductor — there's no central server to orchestrate the upgrade.

### The Solution: Anchor-Based Hash Mapping

When a user upgrades from v1.0 to v1.1:

1. **Install v1.1** alongside v1.0 (both stay installed)
2. **Export** user's authored polls and votes from v1.0
3. **Re-create** polls on v1.1 DHT (they get new action hashes)
4. **Publish migration mappings** — a `MigratedPoll` entry linked from a deterministic migration anchor maps old hashes to new hashes
5. **Re-cast votes** where the target poll's mapping exists on v1.1
6. **Queue pending votes** for polls whose authors haven't upgraded yet
7. **Background retry** — every 60 seconds, check if new mappings appeared and re-cast pending votes

Other users discover the mappings via `get_links` on the migration anchor. As more users upgrade, the v1.1 DHT fills up via gossip.

### Three Tiers of Holochain App Upgrades

| Tier | When | Migration Needed? | Example |
|---|---|---|---|
| **1. Coordinator-only** | Bug fixes, new queries, new link traversals | No — use `admin.updateCoordinators()` | Fix a query bug |
| **2. Additive integrity** | New entry types, new link types | Yes — new DNA hash, new DHT | Adding Flag entry type (v1.1) |
| **3. Breaking integrity** | Changed validation, restructured entries | Yes — with data transformation | Restructuring Poll fields |

**~70% of real-world upgrades are Tier 1** (no migration needed). Tier 2 is what v1.1 demonstrates. Tier 3 follows the same pattern but adds a transformation step.

### Migration Key Files

| File | Purpose |
|---|---|
| `src-tauri/src/migration.rs` | Migration orchestration (export, import, retry loop) |
| `src-tauri/src/dna.rs` | Multi-version install, dual AppWebsocket setup |
| `dna/v1.1/zomes/polls/coordinator/src/lib.rs` | `register_migrated_poll`, `get_migration_mapping` zome functions |
| `dna/v1.1/zomes/polls/integrity/src/lib.rs` | `MigratedPoll` entry type, `MigrationIndex` link type |

### Adding Your Own v1.2

1. **Create `dna/v1.2/`** — copy `dna/v1.1/`, update `network_seed` to `yourapp-network-v1.2` in `dna.yaml`
2. **Add your integrity changes** — new entry types, link types, validation
3. **Update coordinator** — keep all migration functions, add your new zome functions
4. **Update `src-tauri/src/dna.rs`** — add `APP_ID_V1_2`, `HAPP_FILE_V1_2`, update `ACTIVE_APP_ID`
5. **Copy `migration.rs`** — update client references (v1_0 → v1_1 for reads, v1_1 → v1_2 for writes)
6. **Update `build-all.sh`** — add the v1.2 build step
7. **Test** — create data on v1.1, upgrade, verify migration completes

### Staying Visible During Migration

During a migration both DNA cells are active simultaneously — v1.0 continues gossiping and v1.1 is being populated. To keep all content visible during this window, `get_all_polls` in `commands.rs` queries both cells and deduplicates:

1. **Query v1.1** — fetch all polls and all migration mappings (the old→new hash index)
2. **Query v1.0** — fetch all polls, then exclude any whose hash appears in the migration mappings (they're already on v1.1)
3. **Return merged list** — each item carries a `dna_version` field so votes and flag fetches are routed to the correct cell

This means polls are never missing from the UI, even if only one user on the network has upgraded so far. Once a poll's author migrates, the v1.0 copy is automatically hidden and the v1.1 copy takes over.

The same pattern applies to any data type you migrate — the migration mapping index is the key. Deduplication prevents double-showing the same content during the transition window.

### Migration Edge Cases

- **First user on new version**: Their own content migrates fine. References to others' content go to pending (retried every 60s).
- **Content author never upgrades**: References stay pending. Users can "abandon pending votes" to clean up.
- **Crash during migration**: State file (`migration-state.json`) is written after each entry. Restart picks up where it left off.
- **Fresh install (no previous version)**: Installs latest directly. No migration needed, no banner shown.

---

## Reusable Infrastructure

These files work for **any** Holochain + Tauri app with zero or minimal changes:

| File | What It Does | Change Needed |
|---|---|---|
| `src-tauri/src/conductor.rs` | Starts lair-keystore + holochain conductor, waits for readiness, health monitoring | Change ports if running multiple apps |
| `src-tauri/src/lair.rs` | Lair keystore init, socket management, passphrase | None |
| `src-tauri/src/dna.rs` | Multi-version DNA install, dual AppWebsocket, signing credentials. Handles `CellDisabled` on startup by re-enabling the app and retrying credential authorization (Holochain can leave cells disabled after a conductor restart even when the app shows as Running) | Change app IDs and hApp filenames |
| `src-tauri/src/migration.rs` | Migration state machine, export/import/retry pattern | Change entry types and zome names |
| `src/lib/context.ts` | Qwik signals for linked/display state | None |
| `src/lib/sanitize.ts` | XSS prevention for user content | None |
| `src/routes/identity/` | Flowsta identity linking UI | None (if using Flowsta) |

---

## Network Infrastructure (Bootstrap & Signaling)

Holochain apps need a **bootstrap server** for peer discovery and a **signaling server** for NAT traversal. Both are handled by the same binary (`kitsune2-bootstrap-srv`).

**Default (development):** ProofPoll ships pointing at Holochain's public test server:

```rust
// src-tauri/src/conductor.rs
const BOOTSTRAP_URL: &str = "https://dev-test-bootstrap2.holochain.org/";
const SIGNAL_URL: &str = "wss://dev-test-bootstrap2.holochain.org/";
```

This is fine for development and testing, but **for production you must run your own bootstrap server**. The public test server has no uptime guarantees and may be reset at any time.

**Running your own:** See the official Holochain guide:
[Running Network Infrastructure](https://developer.holochain.org/resources/howtos/running-network-infrastructure/)

Then update `BOOTSTRAP_URL` and `SIGNAL_URL` in `src-tauri/src/conductor.rs` to point to your server.

---

## Project Structure

```
ProofPoll/
├── dna/                        # Holochain DNA source
│   ├── zomes/polls/            # v1.0 zomes
│   │   ├── integrity/src/      #   Entry types, validation
│   │   └── coordinator/src/    #   Zome functions (CRUD)
│   ├── workdir/                # v1.0 manifests (dna.yaml, happ.yaml)
│   ├── build.sh                # v1.0 build script
│   └── v1.1/                   # v1.1 DNA (+ flags, migration)
│       ├── zomes/polls/        #   Extended zomes
│       ├── workdir/            #   v1.1 manifests
│       └── build.sh            #   v1.1 build script
├── src-tauri/                  # Tauri v2 Rust backend
│   ├── Cargo.toml              #   Rust dependencies
│   ├── tauri.conf.json         #   App config (name, bundle ID, ports)
│   ├── resources/              #   Built .happ bundles (v1.0 + v1.1)
│   └── src/
│       ├── commands.rs         #   Tauri commands (your app + flags + migration)
│       ├── conductor.rs        #   Conductor lifecycle management
│       ├── dna.rs              #   Multi-version DNA install + WebSocket setup
│       ├── migration.rs        #   DNA migration orchestration
│       ├── lair.rs             #   Lair keystore management
│       └── lib.rs              #   App setup, command registration, startup
├── src/                        # Qwik TypeScript frontend
│   ├── lib/
│   │   ├── holochain.ts        #   Zome call wrappers + types
│   │   ├── context.ts          #   Qwik context signals
│   │   └── sanitize.ts         #   Input sanitization
│   └── routes/
│       ├── layout.tsx          #   Header, conductor status, migration banner
│       ├── index.tsx           #   Content list (+ flag filtering)
│       ├── poll/[id]/          #   Content detail (+ flag button)
│       ├── create/             #   Content creation form
│       └── identity/           #   Flowsta identity linking
├── .env                        # VITE_FLOWSTA_CLIENT_ID
├── build-all.sh                # Build all DNA versions
├── package.json                # Node dependencies
└── vite.config.ts              # Vite + Qwik config (dev port 5174)
```

## License

MIT
