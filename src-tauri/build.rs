fn main() {
    check_auth_material();
    pass_client_id();
    tauri_build::build()
}

/// The Flowsta client id the page is built with (`VITE_FLOWSTA_CLIENT_ID`),
/// handed to the Rust side as `PROOFPOLL_CLIENT_ID` so both name the same
/// app to the Vault. Read the way Vite reads it: the environment first (the
/// release workflow and the test build set it there), then the `.env` file
/// beside `package.json`. Empty when neither has it.
fn pass_client_id() {
    const NAME: &str = "VITE_FLOWSTA_CLIENT_ID";
    println!("cargo:rerun-if-env-changed={NAME}");
    println!("cargo:rerun-if-changed=../.env");
    let from_env = std::env::var(NAME).ok().filter(|v| !v.trim().is_empty());
    let id = from_env.or_else(|| dotenv_value(&std::fs::read_to_string("../.env").ok()?, NAME)).unwrap_or_default();
    println!("cargo:rustc-env=PROOFPOLL_CLIENT_ID={}", id.trim());
}

/// The value of `name` in the text of a `.env` file: `NAME=value`, with
/// optional quotes around the value; comment lines are skipped.
fn dotenv_value(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.trim().strip_prefix("export ").unwrap_or(line.trim()).split_once('=')?;
        if key.trim() != name {
            return None;
        }
        let value = value.trim();
        let unquoted = ['"', '\''].iter().find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q)).unwrap_or(value);
        Some(unquoted.to_string())
    })
}

/// The conductor reads `PROOFPOLL_AUTH_MATERIAL` as padded standard base64:
/// a value without its `=` padding, or in the URL-safe alphabet, makes every
/// conductor start panic with "InvalidPadding". Refuse to build with one
/// rather than ship an app that cannot start.
fn check_auth_material() {
    println!("cargo:rerun-if-env-changed=PROOFPOLL_AUTH_MATERIAL");
    let Ok(m) = std::env::var("PROOFPOLL_AUTH_MATERIAL") else { return };
    if m.is_empty() {
        return;
    }
    let body = m.trim_end_matches('=');
    let alphabet = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '/';
    if m.len() % 4 != 0 || m.len() - body.len() > 2 || !body.chars().all(alphabet) {
        panic!(
            "PROOFPOLL_AUTH_MATERIAL is not padded standard base64 (length {}): the conductor would refuse it",
            m.len()
        );
    }
}
