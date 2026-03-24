# ProofPoll

Verified polls on Holochain with Flowsta identity linking.

ProofPoll is a desktop app (Tauri v2 + Qwik) that runs a local Holochain conductor. Polls and votes are stored on a decentralized DHT — no central server. Identity verification through Flowsta ensures one vote per real person.

**This app is designed to be forked.** Change the entry types, swap polls for reviews or proposals, add your own features — the architecture (conductor management, identity linking, DNA migration, encrypted private data) works for any Holochain app. See [Forking Guide](#forking-guide) below.

## Stack

- **Frontend**: Qwik, TypeScript, Tailwind CSS
- **Backend**: Tauri v2 (Rust), Holochain 0.6.0
- **DNA**: Rust (hdi 0.7.0, hdk 0.6.0)
- **Identity**: Flowsta agent linking via `flowsta-agent-linking` crate
- **Encryption**: lair xsalsa20poly1305 via `crypto_box_xsalsa_by_sign_pub_key`

## Quick Start

```bash
# Prerequisites
# - Rust + wasm32-unknown-unknown target
# - holochain + lair-keystore binaries (v0.6.0)
# - hc CLI: cargo install holochain_cli --version 0.6.0
# - Node.js 18+
# - flowsta-agent-linking repo cloned at ../flowsta-agent-linking/

# Build all DNA versions
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
| v1.2 | `proofpoll-network-v1.2` | + Public/anonymous poll types, voter profiles |
| v1.3 | `proofpoll-network-v1.3` | + Encrypted private data (vote rationale, draft polls) |

All versions are installed side-by-side during migration. All new reads and writes go to v1.3.

## Encrypted Private Data (v1.3)

ProofPoll demonstrates how to store **private data on a public DHT**. Entries are encrypted client-side using lair's xsalsa20poly1305 crypto_box before being committed to the DHT. The data is replicated across peers for resilience, but only the author can decrypt it.

### How it works

1. **Encrypt** — Tauri encrypts plaintext using the agent's Ed25519 signing key (lair converts to x25519 internally)
2. **Store** — the encrypted blob is committed as a public `EncryptedEntry` on the DHT
3. **Gossip** — peers replicate the ciphertext like any other entry — they can see it exists but cannot read it
4. **Decrypt** — only the author's lair-managed private key can decrypt

### What peers see on the DHT

```
cipher: [187, 202, 33, 175, 31, 134, ...]  (opaque bytes)
nonce:  [244, 219, 96, 104, 85, 138, ...]  (random, unique)
hint:   "private"                            (no metadata about content type)
```

No information about whether the entry is a vote rationale, draft poll, or anything else.

### Features

- **Vote rationale** — after voting, add a private note about why you voted that way. Encrypted, only visible to you. Stored on the DHT linked to your vote via `VoteToRationale`.
- **Draft polls** — save polls privately before publishing. Encrypted on the DHT, listed on the Drafts page. Publish when ready (creates a real poll, deletes the draft).

### Key files

| File | Purpose |
|---|---|
| `src-tauri/src/crypto.rs` | `encrypt_to_self` / `decrypt_from_self` via lair crypto_box |
| `dna/v1.3/zomes/polls/integrity/src/lib.rs` | `EncryptedEntry` type, `VoteToRationale` + `AgentDrafts` link types |
| `dna/v1.3/zomes/polls/coordinator/src/lib.rs` | `create_encrypted_entry`, `get_vote_rationale`, `get_my_drafts`, `delete_encrypted_entry` |
| `src-tauri/src/commands.rs` | 6 Tauri commands: save/get rationale, save/get/publish/delete drafts |
| `src/routes/poll/[id]/index.tsx` | Vote rationale UI (private note textarea) |
| `src/routes/drafts/index.tsx` | Drafts page (list, publish, delete) |
| `src/routes/create/index.tsx` | "Save as Draft" button |

### For forking developers

The encryption pattern is generic — `EncryptedEntry` stores any encrypted blob. To add your own private data types:

1. Encrypt your data with `crate::crypto::encrypt_to_self()` in a Tauri command
2. Call `create_encrypted_entry` with a link type that fits your use case
3. Add a new link type in the integrity zome if needed (e.g. `ItemToPrivateNote`)
4. Decrypt with `crate::crypto::decrypt_from_self()` when reading

The `entry_type_hint` field is always `"private"` — no metadata is leaked. Routing is done by link type, not by the hint.

## Community Flagging (v1.1)

Polls can be flagged by signed-in users for: Spam, Misleading, Off Topic, or Inappropriate.

- **Censorship-resistant**: Flags are a UI-layer opinion. The poll and all votes remain on the DHT forever. No data is deleted.
- **Sybil-resistant**: One flag per Flowsta identity per poll (same deduplication as votes).
- **Configurable threshold**: Polls with >= `FLAG_HIDE_THRESHOLD` flags (default 3) are hidden from the default view. Users can toggle "Show flagged" to see them.
- **Forking developers**: Change `FLAG_HIDE_THRESHOLD` in the coordinator zome to suit your community size.

---

## Forking Guide

This section is for developers (and AIs) who want to fork ProofPoll into a completely different app — a review platform, a task tracker, a social feed, anything. The architecture is app-agnostic; the poll/vote specifics are easy to swap out.

### Step 1: Rename Everything

These identifiers **must** change or your app will conflict with ProofPoll:

| What | Where | Current Value | Change To |
|---|---|---|---|
| Bundle ID | `src-tauri/tauri.conf.json` | `com.proofpoll.app` | `com.yourcompany.yourapp` |
| Product name | `src-tauri/tauri.conf.json` | `ProofPoll` | `YourApp` |
| Rust crate name | `src-tauri/Cargo.toml` | `proofpoll` | `yourapp` |
| npm package name | `package.json` | `proofpoll` | `yourapp` |
| DNA names | `dna/*/workdir/dna.yaml` | `proofpoll_v1_*` | `yourapp_v1_*` |
| Network seeds | `dna/*/workdir/dna.yaml` | `proofpoll-network-v1.*` | `yourapp-network-v1.*` |
| hApp names | `dna/*/workdir/happ.yaml` | `proofpoll_v1_*_happ` | `yourapp_v1_*_happ` |
| hApp role | `dna/*/workdir/happ.yaml` | `proofpoll` | `yourapp` |

Then update these Rust constants:

| File | What to change |
|---|---|
| `src-tauri/src/dna.rs` | `APP_ID_V1_*` and `HAPP_FILE_V1_*` constants |
| `src-tauri/src/commands.rs` | `ROLE_NAME` constant |
| `src-tauri/src/migration.rs` | `ROLE_NAME` constant |
| `src-tauri/src/dna.rs` | `"proofpoll"` origin string in WebSocket connects |

Update build scripts (`dna/*/build.sh`, `build-all.sh`) — change hApp filenames.

**Critical**: The `network_seed` in `dna.yaml` determines which DHT your app joins. Two apps with the same network seed share a DHT. Always use a unique seed.

### Step 2: Replace Entry Types

ProofPoll's data model is polls and votes. Replace these with your own.

**Integrity zome** (latest version, currently `dna/v1.3/zomes/polls/integrity/src/lib.rs`):

```rust
// REPLACE these with your entry types:
pub struct Poll { ... }     // → pub struct Review { ... }
pub struct Vote { ... }     // → pub struct Rating { ... }
pub struct Flag { ... }     // Keep or adapt for your content type

// KEEP these as-is (infrastructure):
pub struct MigratedPoll { ... }   // Rename "Poll" to your type but keep the structure
pub struct EncryptedEntry { ... } // Generic — works for any private data
```

**Coordinator zome** — replace the zome functions. The patterns are reusable:
- `create_poll` → `create_review` (same anchor + link pattern)
- `cast_vote` → `submit_rating` (same one-per-agent enforcement pattern)
- `get_all_polls` → `get_all_reviews` (same anchor query pattern)
- `flag_poll` → `flag_review` (same pattern, just rename)

Keep the migration functions and encrypted entry functions — they're generic.

### Step 3: Update Tauri Commands

`src-tauri/src/commands.rs` has mirror types and Tauri commands for each zome function.

**Replace** the poll/vote/flag Rust structs and commands with your own. The pattern is always:
1. Define a response struct (serializable)
2. `#[tauri::command]` function that locks the AppWebsocket, encodes payload, calls `call_zome`, decodes result

**Keep** these as-is (infrastructure):
- `AppState`, `call_zome()`, `try_reenable_app()`, `friendly_error()`, `decode_entry()`
- `get_app_status`, `get_export_data` (adapt the data it exports)
- Identity link commands (`commit_identity_link`, `get_identity_link`, etc.)
- Encrypted entry commands (`save_vote_rationale`, `save_draft_poll`, etc. — adapt names)
- Migration status commands (`get_migration_status`, `abandon_pending_votes`)

**Register new commands** in `src-tauri/src/lib.rs` → `invoke_handler(tauri::generate_handler![...])`.

### Step 4: Update Frontend

**`src/lib/holochain.ts`** — Replace poll/vote/flag TypeScript types and `invoke()` wrappers with your own. Keep the identity, migration, and encrypted entry functions.

**`src/routes/`** — Replace the pages:
- `index.tsx` → Your content list page
- `create/index.tsx` → Your content creation form
- `poll/[id]/index.tsx` → Your content detail page

**Keep as-is:**
- `layout.tsx` — Conductor startup, header, migration banner (just rename "ProofPoll")
- `identity/index.tsx` — Flowsta identity linking page
- `drafts/index.tsx` — Encrypted drafts page (adapt for your draft type)
- `src/lib/context.ts` — Qwik signals for linked state
- `src/lib/sanitize.ts` — XSS prevention

### Step 5: Update Migration

`src-tauri/src/migration.rs` exports data from the previous version and re-creates it on the current version. The source client is clearly marked — change one line to point to your previous version's client field.

Replace the entry types and zome function names with your own. The orchestration pattern (export → create → register mapping → cast → retry loop) is identical for any data model.

The state file name is auto-generated from `ACTIVE_APP_ID` — no hardcoded strings to update.

---

## Flowsta Integration Points

ProofPoll uses [Flowsta](https://flowsta.com) for decentralized identity verification. If you want to use Flowsta in your fork, keep these as-is and just change the client_id. If you want a different identity system (or none), remove them.

### Setup

1. Register your app at [dev.flowsta.com](https://dev.flowsta.com) to get a `client_id`
2. Update `.env`: `VITE_FLOWSTA_CLIENT_ID=your_client_id_here`
3. Clone `flowsta-agent-linking` at `../flowsta-agent-linking/` (referenced by build scripts)
4. Keep the `agent_linking_integrity` and `agent_linking` zomes in your `dna.yaml`
5. Update the `appName` parameter in `linkFlowstaIdentity()` calls to your app name (shown in the Vault approval dialog)

### Scopes

Scopes control which Flowsta profile fields your app can access. They are configured per-app at [dev.flowsta.com](https://dev.flowsta.com) and are shown to the user in the Flowsta Vault approval dialog when they first sign in. The Vault enforces them — it only exposes data fields the user actually approved, regardless of what the app requests at runtime.

**ProofPoll requests these scopes:**

| Scope | What it provides | Why ProofPoll uses it |
|---|---|---|
| `openid` | Basic identity (implicit) | Required by all apps — not shown to the user |
| `did` | Decentralized identifier | Unique identity for sybil resistance |
| `public_key` | Holochain agent pub key | Links the Vault identity to the DHT entry |
| `holochain` | Holochain identity attestation | Required for `agent_linking` zome ceremony |
| `display_name` | The user's display name | Shown in the app header and voter chips |
| `username` | The user's @username | Displayed on the identity page |
| `profile_picture` | Avatar URL | Shown in the app header and voter chips |

The `display_name`, `username`, and `profile_picture` scopes are optional — ProofPoll requests them for a friendlier UI. If your fork has no use for profile data, remove them from your app's scope configuration at dev.flowsta.com.

**Configuring scopes for your fork:**

1. Register your app at [dev.flowsta.com](https://dev.flowsta.com) and create a new application
2. In the app settings, select the scopes your app needs
3. Copy your `client_id` into `.env` as `VITE_FLOWSTA_CLIENT_ID`
4. The selected scopes are fetched fresh from the Flowsta API each time a user goes through the linking flow, so scope changes take effect immediately — no app rebuild needed

**What the user sees:** The Vault approval dialog lists every scope (except `openid`) in plain language before the user approves. The Vault will only serve those fields on `GET /status` at `localhost:27777`.

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

### Automatic Backups

ProofPoll backs up the user's authored data to Flowsta Vault's encrypted local storage every 60 minutes. The backup includes public data (polls, votes), private data (vote rationales, drafts — decrypted), and cryptographic keys (CAL compliance). Users can view, export, and delete their backups from the Vault's **Your Data** page.

- Backups work even when the Vault is locked (after first unlock in the session)
- Each backup creates a new timestamped snapshot (up to 10 per app, oldest auto-rotated)
- Only the current user's data is backed up (not the entire DHT)
- Encrypted entries are decrypted before export so the backup is human-readable

**Key files:**
- `src/routes/layout.tsx` — `startAutoBackup()` call with `getData` function
- `src-tauri/src/commands.rs` — `get_export_data` command (queries and decrypts all user data)

**Keeping backups in sync with your data model:** Every time you add new entry types or zome functions that create user data, update `get_export_data` to include that data in the backup. If you don't, users will have incomplete exports. This applies to both public entries (new content types) and encrypted entries (new private data types). The backup should always reflect everything the user has created.

**Tips for human-readable exports:**
- Include `_readme` fields at each section explaining what the data is
- Use human-readable field names (`poll_title`, `voted_for`) not just hashes
- Group related data together (e.g. `private_data` section for all encrypted content)
- Include context: a vote rationale is more useful when it shows the poll title and which option was chosen
- Decrypt all private data — the backup file is already protected by the Vault's own encryption

**For forks:** Update `get_export_data` in `commands.rs` to export your own entry types. The `appName` parameter in `layout.tsx` controls how your app appears in the Vault's Your Data page.

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

When a user upgrades to a new version:

1. **Install new version** alongside the old (both stay installed)
2. **Export** user's authored content and actions from the old DHT
3. **Re-create** content on the new DHT (entries get new action hashes)
4. **Publish migration mappings** — a `MigratedPoll` entry linked from a deterministic migration anchor maps old hashes to new hashes
5. **Re-cast actions** (votes, etc.) where the target content's mapping exists
6. **Queue pending actions** for content whose authors haven't upgraded yet
7. **Background retry** — every 60 seconds, check if new mappings appeared and retry

Other users discover the mappings via `get_links` on the migration anchor. As more users upgrade, the new DHT fills up via gossip.

### Three Tiers of Holochain App Upgrades

| Tier | When | Migration Needed? | Example |
|---|---|---|---|
| **1. Coordinator-only** | Bug fixes, new queries, new link traversals | No — use `admin.updateCoordinators()` | Fix a query bug |
| **2. Additive integrity** | New entry types, new link types | Yes — new DNA hash, new DHT | Adding EncryptedEntry (v1.3) |
| **3. Breaking integrity** | Changed validation, restructured entries | Yes — with data transformation | Restructuring Poll fields |

**~70% of real-world upgrades are Tier 1** (no migration needed). Tier 2 is what ProofPoll demonstrates across v1.0→v1.3. Tier 3 follows the same pattern but adds a transformation step.

### Votes Survive Migration

During migration, polls from older versions remain visible and functional:

- `get_all_polls` queries ALL installed versions and deduplicates using migration mappings
- Each poll carries a `dna_version` field so votes and flags are routed to the correct cell
- If a poll author hasn't migrated, their poll stays on the old DHT — votes cast on it go to the old cell
- Once the author migrates, the old copy is hidden and the new copy takes over

No votes are ever lost. Users on different versions can still interact with content on the version where it lives.

### Migration Key Files

| File | Purpose |
|---|---|
| `src-tauri/src/migration.rs` | Migration orchestration (export, import, retry loop). Source client clearly marked for forkers |
| `src-tauri/src/dna.rs` | Multi-version install, AppWebsocket setup per version |
| `src-tauri/src/commands.rs` | `get_all_polls` multi-version merge with chained dedup |
| `dna/v1.3/zomes/polls/coordinator/src/lib.rs` | `register_migrated_poll`, `get_migration_mapping` zome functions |
| `dna/v1.3/zomes/polls/integrity/src/lib.rs` | `MigratedPoll` entry type, `MigrationIndex` link type |

### Adding Your Own Version

1. **Create `dna/vX.Y/`** — copy the latest version, update `network_seed` in `dna.yaml`
2. **Add your integrity changes** — new entry types, link types, validation
3. **Update coordinator** — keep all migration + encrypted entry functions, add your new zome functions
4. **Update `src-tauri/src/dna.rs`** — add `APP_ID_VX_Y`, `HAPP_FILE_VX_Y`, update `ACTIVE_APP_ID`, add `app_client_vX_Y` to `AppState`
5. **Update `src-tauri/src/migration.rs`** — change the source client field (one line, clearly marked with `// FORKING`)
6. **Update `src-tauri/src/commands.rs`** — add your previous version to the `older_versions` array in `get_all_polls`
7. **Update `build-all.sh`** — add the new build step
8. **Test** — create data on the old version, upgrade, verify migration completes and all content is visible

The migration state file is auto-generated from `ACTIVE_APP_ID` — no hardcoded strings to update.

### Staying Visible During Migration

During a migration all DNA cells are active simultaneously. `get_all_polls` queries every installed version and deduplicates:

1. **Collect migration mappings** from ALL versions into one set (chains across multi-hop migrations)
2. **Query each version** — skip any poll whose hash appears in the migrated set
3. **Return merged list** — each item carries `dna_version` so votes and flags are routed to the correct cell

This means content is never missing from the UI, even if only one user on the network has upgraded so far.

### Migration Edge Cases

- **First user on new version**: Their own content migrates fine. References to others' content go to pending (retried every 60s).
- **Content author never upgrades**: Content stays on the old DHT and remains visible. Actions (votes) cast on it go to the old cell. Users can "abandon pending votes" to clean up.
- **Crash during migration**: State file is written after each entry. Restart picks up where it left off.
- **Fresh install (no previous version)**: Installs latest directly. No migration needed.

---

## Reusable Infrastructure

These files work for **any** Holochain + Tauri app with zero or minimal changes:

| File | What It Does | Change Needed |
|---|---|---|
| `src-tauri/src/conductor.rs` | Starts lair-keystore + holochain conductor, waits for readiness, health monitoring | Change ports if running multiple apps |
| `src-tauri/src/lair.rs` | Lair keystore init, socket management, passphrase | None |
| `src-tauri/src/crypto.rs` | Encrypt/decrypt via lair's xsalsa20poly1305 crypto_box | None |
| `src-tauri/src/dna.rs` | Multi-version DNA install, AppWebsocket per version, signing credentials with CellDisabled recovery | Change app IDs and hApp filenames |
| `src-tauri/src/migration.rs` | Migration state machine, export/import/retry pattern. Auto-versioned state file | Change entry types, zome names, and source client field |
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
│   ├── v1.1/                   # v1.1 DNA (+ flags, migration)
│   ├── v1.2/                   # v1.2 DNA (+ public/anonymous polls)
│   └── v1.3/                   # v1.3 DNA (+ encrypted private data)
│       ├── zomes/polls/
│       │   ├── integrity/src/  #   EncryptedEntry, VoteToRationale, AgentDrafts
│       │   └── coordinator/src/#   Encrypted entry CRUD + existing functions
│       ├── workdir/            #   v1.3 manifests
│       └── build.sh            #   v1.3 build script
├── src-tauri/                  # Tauri v2 Rust backend
│   ├── Cargo.toml              #   Rust dependencies
│   ├── tauri.conf.json         #   App config (name, bundle ID, ports)
│   ├── resources/              #   Built .happ bundles (v1.0 through v1.3)
│   └── src/
│       ├── commands.rs         #   Tauri commands (app + flags + encrypted entries + migration)
│       ├── conductor.rs        #   Conductor lifecycle management
│       ├── crypto.rs           #   Lair-based encryption (xsalsa20poly1305)
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
│       ├── poll/[id]/          #   Content detail (+ flag + vote rationale)
│       ├── create/             #   Content creation form (+ save as draft)
│       ├── drafts/             #   Encrypted draft polls page
│       └── identity/           #   Flowsta identity linking
├── .env                        # VITE_FLOWSTA_CLIENT_ID
├── build-all.sh                # Build all DNA versions
├── package.json                # Node dependencies
└── vite.config.ts              # Vite + Qwik config (dev port 5174)
```

## License

MIT
