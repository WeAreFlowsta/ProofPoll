fn main() {
    check_auth_material();
    tauri_build::build()
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
