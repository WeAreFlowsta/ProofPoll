//! `proofpoll-carrier`: one pass that carries ProofPoll's public records
//! (polls, votes, flags and agent links) from DNA v1.3 (Holochain 0.6) and
//! v1.4 (0.7) to v1.5 on a node, each with its proof (the original record
//! as its author signed it, which every v1.5 peer checks), then exits. Run
//! it again to carry what arrived since: what is there already is skipped.
//!
//! It writes on v1.5 only: the carries, and the removal of a copy it made
//! when the original's author deleted the original. On each conductor it
//! also adds what any client needs: a signing-credential grant on the
//! node's own cell (an app interface is reused when one answers to its
//! origin). It prints counts only, one JSON line.
//!
//! ```text
//! proofpoll-carrier [--admin-06 localhost:4444] [--admin-07 localhost:4445]
//!   [--origin flowsta-agent] [--dry-run] [--max-writes 500]
//!   [--pause-ms 200] [--min-free-mb 150]
//! ```
//! `--dry-run` reads and counts, and writes nothing (not even the grants'
//! zome calls beyond reads). Exit status: 0 when the pass ran (whatever it
//! carried), 2 when it stopped early (memory or the write limit), 1 when it
//! could not run.

#[allow(dead_code)]
#[path = "../../src-tauri/src/hc.rs"]
mod hc;

#[allow(dead_code)]
#[path = "../../src-tauri/src/carried.rs"]
mod carried;

mod node;

fn arg(args: &[String], name: &str) -> Option<String> {
    let flag = format!("--{name}");
    args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).filter(|v| !v.starts_with("--")).cloned()
}

fn number(args: &[String], name: &str, default: u64) -> Result<u64, String> {
    match arg(args, name) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| format!("--{name} takes a number")),
    }
}

fn options() -> Result<node::Options, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("proofpoll-carrier [--admin-06 localhost:4444] [--admin-07 localhost:4445] [--origin flowsta-agent] [--dry-run] [--max-writes 500] [--pause-ms 200] [--min-free-mb 150]");
        std::process::exit(0);
    }
    Ok(node::Options {
        admin_06: arg(&args, "admin-06").unwrap_or_else(|| "localhost:4444".into()),
        admin_07: arg(&args, "admin-07").unwrap_or_else(|| "localhost:4445".into()),
        origin: arg(&args, "origin").unwrap_or_else(|| "flowsta-agent".into()),
        dry_run: args.iter().any(|a| a == "--dry-run"),
        max_writes: number(&args, "max-writes", 500)?,
        pause_ms: number(&args, "pause-ms", 200)?,
        min_free_mb: number(&args, "min-free-mb", 150)?,
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let opts = match options() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    match node::run(&opts).await {
        Ok(report) => {
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
            std::process::exit(if report.stopped.is_some() { 2 } else { 0 });
        }
        Err(e) => {
            eprintln!("carrier: {e}");
            std::process::exit(1);
        }
    }
}
