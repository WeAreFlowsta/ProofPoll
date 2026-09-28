//! Finding the right Flowsta Vault on this computer.
//!
//! The Vault binds 27777, or the next port up when that is taken. Loopback
//! ports are shared by every user account on a computer: on 2026-09-25 a Mac
//! had the production Vault of ANOTHER account on 27777 and a staging build
//! on 27778, so "the first port that answers" was the wrong Vault - and the
//! Tier-1 identity gate FAILED OPEN when 27777 was empty and the person's own
//! Vault sat on 27778. So: ask all three ports at once, drop any Vault whose
//! listening process belongs to another OS user, and prefer the unlocked one,
//! then an initialized (locked) one. Same rule as Your Own AI 0.8.0.
//!
//! When the owner of a listener cannot be read (a tool missing, a localized
//! netstat) the Vault is kept - never lose the person's own Vault to a failed
//! check. Verdicts are cached per port for a minute.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const VAULT_PORTS: [u16; 3] = [27777, 27778, 27779];

fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| reqwest::Client::builder().build().expect("reqwest client"))
        .clone()
}

fn cached_port() -> &'static Mutex<Option<u16>> {
    static PORT: OnceLock<Mutex<Option<u16>>> = OnceLock::new();
    PORT.get_or_init(|| Mutex::new(None))
}

/// What one Vault answered. `port` is where it lives.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VaultProbe {
    pub port: u16,
    pub url: String,
    pub unlocked: bool,
    pub initialized: bool,
    pub agent_pub_key: Option<String>,
    pub display_name: Option<String>,
    pub profile_picture: Option<String>,
}

impl VaultProbe {
    fn from_status(port: u16, v: &serde_json::Value) -> VaultProbe {
        VaultProbe {
            port,
            url: format!("http://127.0.0.1:{port}"),
            unlocked: v["unlocked"].as_bool().unwrap_or(false),
            initialized: v["initialized"].as_bool().unwrap_or(true),
            agent_pub_key: v["agent_pub_key"].as_str().map(String::from),
            display_name: v["display_name"].as_str().map(String::from),
            profile_picture: v["profile_picture"].as_str().map(String::from),
        }
    }
}

/// Every Vault of THIS OS user answering on this computer, all three ports
/// asked at once.
async fn probe_vaults(timeout: Duration) -> Vec<VaultProbe> {
    let client = http();
    let one = |port: u16| {
        let client = client.clone();
        async move {
            let resp = client
                .get(format!("http://127.0.0.1:{port}/status"))
                .timeout(timeout)
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let v = resp.json::<serde_json::Value>().await.ok()?;
            Some(VaultProbe::from_status(port, &v))
        }
    };
    let (a, b, c) = tokio::join!(one(VAULT_PORTS[0]), one(VAULT_PORTS[1]), one(VAULT_PORTS[2]));
    let mut mine = Vec::new();
    for p in [a, b, c].into_iter().flatten() {
        if vault_is_mine(p.port).await {
            mine.push(p);
        }
    }
    mine
}

/// Unlocked first, then initialized (locked), then any; ties to the lower port.
fn pick_vault(answers: &[VaultProbe]) -> Option<&VaultProbe> {
    answers
        .iter()
        .find(|p| p.unlocked)
        .or_else(|| answers.iter().find(|p| p.initialized))
        .or_else(|| answers.first())
}

/// The Vault to talk to right now, or None when none of this user's Vaults
/// answers. Remembers the port for `vault_base_url`.
pub(crate) async fn find_vault(timeout: Duration) -> Option<VaultProbe> {
    let answers = probe_vaults(timeout).await;
    let best = pick_vault(&answers).cloned();
    *cached_port().lock().unwrap() = best.as_ref().map(|p| p.port);
    best
}

/// Base URL for a Vault call: a fresh sweep when possible, else the last
/// port that answered, else the default port (the caller's own error copy
/// then says the Vault is unreachable).
pub(crate) async fn vault_base_url() -> String {
    if let Some(p) = find_vault(Duration::from_millis(1500)).await {
        return p.url;
    }
    let port = cached_port().lock().unwrap().unwrap_or(VAULT_PORTS[0]);
    format!("http://127.0.0.1:{port}")
}

/// Tauri command for the webview: the same answer the Rust gates use, so
/// the page can never talk to a different Vault than the identity guard.
#[tauri::command]
pub async fn probe_vault() -> Option<VaultProbe> {
    find_vault(Duration::from_millis(2000)).await
}

/// Relaunch ProofPoll. At launch the profile is picked from the identity the
/// Vault has unlocked (`profiles::select_profile_root`), so after a switch
/// this is how ProofPoll moves onto the new identity's own profile instead
/// of staying read-only under the old name.
#[tauri::command]
pub fn restart_app(app: tauri::AppHandle, state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>) {
    log::info!("Restarting into the Vault's current identity");
    // The relaunch reads this once: bring the window back to the front
    // (the OS hands focus to whatever was behind us, usually the Vault)
    // and, if the profile it opens is not signed in yet, start the sign-in.
    if let Err(e) = std::fs::write(relaunch_marker_path(&state.device_root), b"1") {
        log::warn!("relaunch marker not written: {}", e);
    }
    app.restart();
}

pub(crate) fn relaunch_marker_path(device_root: &std::path::Path) -> std::path::PathBuf {
    device_root.join("relaunch-into-identity")
}

/// True once per relaunch that "Open as this identity" asked for: the
/// marker is consumed here so a later normal launch reads false.
pub(crate) fn take_relaunch_marker(device_root: &std::path::Path) -> bool {
    let p = relaunch_marker_path(device_root);
    if p.exists() {
        let _ = std::fs::remove_file(&p);
        true
    } else {
        false
    }
}

// ── Whose Vault is it? ────────────────────────────────────────────

async fn vault_is_mine(port: u16) -> bool {
    static CACHE: Mutex<Vec<(u16, bool, Instant)>> = Mutex::new(Vec::new());
    if let Ok(c) = CACHE.lock() {
        if let Some((_, v, _)) = c.iter().find(|(p, _, at)| *p == port && at.elapsed() < Duration::from_secs(60)) {
            return *v;
        }
    }
    let read = tokio::task::spawn_blocking(move || listener_is_mine(port)).await.ok().flatten();
    let mine = read.unwrap_or(true);
    if let Ok(mut c) = CACHE.lock() {
        let said_before = c.iter().any(|(p, v, _)| *p == port && !*v);
        c.retain(|(p, _, _)| *p != port);
        c.push((port, mine, Instant::now()));
        if !mine && !said_before {
            log::info!("[vault] the Vault on port {port} runs as another user of this computer - ignored");
        }
    }
    mine
}

/// Some(true/false) when the owner of the process listening on `port` could
/// be read, None when it could not.
#[cfg(target_os = "linux")]
fn listener_is_mine(port: u16) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let me = std::fs::metadata("/proc/self").ok()?.uid();
    let mut seen: Option<bool> = None;
    for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(f) {
            for uid in proc_net_listen_uids(&text, port) {
                if uid == me {
                    return Some(true);
                }
                seen = Some(false);
            }
        }
    }
    seen
}

#[cfg(target_os = "macos")]
fn listener_is_mine(port: u16) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let me = std::fs::metadata(std::env::var_os("HOME")?).ok()?.uid();
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fu"])
        .output()
        .ok()?;
    let uids = lsof_uids(&String::from_utf8_lossy(&out.stdout));
    if uids.contains(&me) {
        return Some(true);
    }
    if !uids.is_empty() {
        return Some(false);
    }
    // Unprivileged lsof does not list other users' processes: a Vault that
    // answers on the port but is not listed runs as someone else.
    if out.status.code() == Some(1) && out.stderr.is_empty() {
        return Some(false);
    }
    None
}

#[cfg(target_os = "windows")]
fn listener_is_mine(port: u16) -> Option<bool> {
    use std::os::windows::process::CommandExt;
    const NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    let pid = netstat_listening_pids(&String::from_utf8_lossy(&out.stdout), &port.to_string())
        .into_iter()
        .next()?;
    let list = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/V", "/FO", "CSV", "/NH"])
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    let user = tasklist_user(&String::from_utf8_lossy(&list.stdout))?;
    let name = std::env::var("USERNAME").ok()?;
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let me_full = format!("{domain}\\{name}");
    Some(user.eq_ignore_ascii_case(&me_full) || user.eq_ignore_ascii_case(&name))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn listener_is_mine(_port: u16) -> Option<bool> {
    None
}

/// uids of the processes LISTENING on `port` in a /proc/net/tcp(6) table.
#[allow(dead_code)]
pub(crate) fn proc_net_listen_uids(text: &str, port: u16) -> Vec<u32> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let c: Vec<&str> = line.split_whitespace().collect();
            if c.len() < 8 || c[3] != "0A" {
                return None; // 0A = LISTEN
            }
            let p = u16::from_str_radix(c[1].rsplit(':').next()?, 16).ok()?;
            if p != port {
                return None;
            }
            c[7].parse().ok()
        })
        .collect()
}

/// uids in `lsof -Fu` output (lines "u<uid>").
#[allow(dead_code)]
pub(crate) fn lsof_uids(text: &str) -> Vec<u32> {
    text.lines().filter_map(|l| l.strip_prefix('u')?.trim().parse().ok()).collect()
}

/// The pids LISTENING on `port` in `netstat -ano` output; a line whose
/// FOREIGN address is the port is one of our own client connections.
#[allow(dead_code)]
pub(crate) fn netstat_listening_pids(text: &str, port: &str) -> Vec<String> {
    let suffix = format!(":{port}");
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 5 || cols[0] != "TCP" {
                return None;
            }
            if !cols[1].ends_with(&suffix) || cols[3] != "LISTENING" {
                return None;
            }
            Some(cols[4].to_string())
        })
        .collect()
}

/// The "User Name" column of one `tasklist /V /FO CSV /NH` line.
#[allow(dead_code)]
pub(crate) fn tasklist_user(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with('"'))?;
    let cols: Vec<&str> = line.trim().trim_matches('"').split("\",\"").collect();
    cols.get(6).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(port: u16, unlocked: bool, initialized: bool) -> VaultProbe {
        VaultProbe { port, url: format!("http://127.0.0.1:{port}"), unlocked, initialized, agent_pub_key: None, display_name: None, profile_picture: None }
    }

    #[test]
    fn unlocked_beats_locked_beats_fresh_and_lower_port_breaks_ties() {
        let a = [probe(27777, false, true), probe(27778, true, true)];
        assert_eq!(pick_vault(&a).unwrap().port, 27778);
        let b = [probe(27777, false, false), probe(27779, false, true)];
        assert_eq!(pick_vault(&b).unwrap().port, 27779);
        let c = [probe(27777, true, true), probe(27778, true, true)];
        assert_eq!(pick_vault(&c).unwrap().port, 27777);
        assert!(pick_vault(&[]).is_none());
    }

    #[test]
    fn proc_net_names_the_listener_uid_for_the_port() {
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:6C81 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 11111 1
   1: 0100007F:6C82 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 22222 1
   2: 0100007F:D1F4 0100007F:6C81 01 00000000:00000000 00:00000000 00000000  1000        0 33333 1
";
        assert_eq!(proc_net_listen_uids(text, 27777), vec![501]);
        assert_eq!(proc_net_listen_uids(text, 27778), vec![1000]);
        assert!(proc_net_listen_uids(text, 27779).is_empty());
    }

    #[test]
    fn lsof_netstat_and_tasklist_parsers() {
        assert_eq!(lsof_uids("p123\nu501\nf12\nu502\n"), vec![501, 502]);
        let ns = "  Proto  Local Address          Foreign Address        State           PID
  TCP    127.0.0.1:27777        0.0.0.0:0              LISTENING       4242
  TCP    127.0.0.1:53012        127.0.0.1:27777        ESTABLISHED     999
";
        assert_eq!(netstat_listening_pids(ns, "27777"), vec!["4242"]);
        let tl = "\"flowsta-vault.exe\",\"4242\",\"Console\",\"1\",\"12,345 K\",\"Running\",\"PC\\eric\",\"0:00:01\",\"Flowsta Vault\"\n";
        assert_eq!(tasklist_user(tl).as_deref(), Some("PC\\eric"));
    }
}
