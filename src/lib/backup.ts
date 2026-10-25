// Write-triggered Vault backups.
//
// The SDK's `getData`-based `startAutoBackup` signature (the one apps whose
// Rust side owns the conductor must use) has no write hook - it backs up
// once on start and then hourly. On its own that means a poll created two
// minutes after launch doesn't reach the user's Vault until the next launch
// or the next hourly tick. This module adds the missing half: every
// successful DHT write schedules a debounced backup, mirroring the SDK's V2
// defaults (30 s debounce, coalesced, at most one in flight).
//
// Forks: call `requestBackupSoon()` after every successful write - the
// wrappers in `holochain.ts` already do, so writes routed through them are
// covered automatically. Per-command scheduling is how backup freshness
// drifts; keep new writes on the wrapper pattern.
import { invoke } from "@tauri-apps/api/core";

const DEBOUNCE_MS = 30_000;

/**
 * Tell the Rust side what a backup attempt did. It writes proofpoll.log and
 * keeps the last outcome for the Identity page - the console alone left a
 * stale backup date unexplained (Windows, 2026-10-25). Fire and forget.
 */
export function reportBackupOutcome(
  ok: boolean,
  message: string,
  bytes?: number,
): void {
  void invoke("report_backup_outcome", { ok, message, bytes: bytes ?? null }).catch(
    () => {},
  );
}

/** What the last attempt did, as the Rust side remembers it. */
export interface BackupOutcome {
  ok: boolean;
  message: string;
  bytes: number | null;
  at: number;
}

export async function getLastBackupOutcome(): Promise<BackupOutcome | null> {
  try {
    return await invoke<BackupOutcome | null>("get_last_backup_outcome");
  } catch {
    return null;
  }
}

let enabled = false;
let timer: ReturnType<typeof setTimeout> | null = null;
let inFlight = false;
let queued = false;

/**
 * Gate the triggers on a CONFIRMED identity - the layout flips this with the
 * same signal that starts/stops the hourly auto-backup. While disabled,
 * requests are dropped (the write itself already refused if unconfirmed;
 * this only skips the redundant backup attempt).
 */
export function setBackupTriggersEnabled(on: boolean) {
  enabled = on;
  if (!on && timer) {
    clearTimeout(timer);
    timer = null;
  }
}

/** Schedule a backup soon. Debounced; safe to call on every write. */
export function requestBackupSoon() {
  if (!enabled) return;
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => void runBackup(), DEBOUNCE_MS);
}

async function runBackup() {
  timer = null;
  if (inFlight) {
    // A backup is mid-flight; run once more after it finishes so the write
    // that arrived meanwhile isn't lost until the hourly tick.
    queued = true;
    return;
  }
  inFlight = true;
  try {
    const clientId = import.meta.env.VITE_FLOWSTA_CLIENT_ID;
    // The Rust side gates this payload twice: identity match, and the
    // escrow guard (never overwrite a backup escrowing a foreign seed).
    const payload = await invoke("build_canonical_backup", { clientId });
    const { backupToVault, wouldOverwriteNonEmptyBackup } = await import(
      "@flowsta/holochain"
    );
    // Same empty-payload guard startAutoBackup applies internally - posting
    // directly with backupToVault means running it ourselves.
    if (await wouldOverwriteNonEmptyBackup({ clientId }, payload)) {
      console.warn("[ProofPoll] write-triggered backup skipped: empty payload");
      reportBackupOutcome(
        false,
        "Nothing of yours to back up yet - your Vault keeps the backup it has.",
      );
      return;
    }
    const result = await backupToVault(
      { clientId, appName: "ProofPoll" },
      payload,
    );
    console.log(`[ProofPoll] Vault backup after write: ${result.dataSize} bytes`);
    reportBackupOutcome(true, "saved", result.dataSize);
  } catch (e: any) {
    // Held or failed backups are a skip, not a failure - the hourly
    // auto-backup and the next write both retry.
    const message = e?.message ?? String(e);
    console.warn("[ProofPoll] write-triggered backup skipped:", message);
    reportBackupOutcome(false, message);
  } finally {
    inFlight = false;
    if (queued) {
      queued = false;
      requestBackupSoon();
    }
  }
}
