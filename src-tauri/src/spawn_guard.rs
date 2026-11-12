//! A guard against console windows on Windows. The app is a GUI process, so
//! every console program it starts gets a window of its own unless the call
//! says otherwise: `creation_flags(CREATE_NO_WINDOW)` for a tool call,
//! `crate::process_ext::SidecarCommand` for a sidecar (it hides its own).
//! This test is what stops the next start added without one.

/// Every `Command::new(` in the crate (sidecar commands aside) that has no
/// `creation_flags` within the 12 lines from it, by file. Each one is here
/// for a reason: it never runs on Windows, it is test code, or it opens a
/// window app.
const WITHOUT_THE_FLAG: &[(&str, usize, &str)] = &[
    ("commands.rs", 3, "opens Flowsta Vault, a window app, on each platform"),
    ("instance_guard.rs", 3, "tests"),
    ("lair.rs", 1, "a test"),
    ("process_ext.rs", 2, "the Unix sidecar start and kill"),
    ("vault_probe.rs", 1, "lsof: macOS"),
];

#[test]
fn no_new_program_start_without_the_no_window_flag() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found: Vec<(String, usize)> = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name == "spawn_guard.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let n = (0..lines.len())
            .filter(|&i| lines[i].contains("Command::new(") && !lines[i].contains("SidecarCommand::new(") && !lines[i].trim_start().starts_with("//"))
            .filter(|&i| {
                let window = lines[i..(i + 12).min(lines.len())].join("\n");
                !window.contains("creation_flags")
            })
            .count();
        if n > 0 {
            found.push((name, n));
        }
    }
    found.sort();
    let known: Vec<(String, usize)> = WITHOUT_THE_FLAG.iter().map(|(f, n, _)| (f.to_string(), *n)).collect();
    assert_eq!(
        found, known,
        "a program is started without the no-window flag (or one was removed). On Windows that is a console \
         window on the screen. Give the call creation_flags(CREATE_NO_WINDOW) under cfg(windows); only if it can never run on \
         Windows, change the count in spawn_guard.rs and say why."
    );
}
