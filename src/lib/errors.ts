/// Format an error caught from a Tauri `invoke()` call.
///
/// Tauri rejects with the raw string when the Rust command returns
/// `Result<_, String>`, so `e` is typically a string, not an Error
/// object. The old `e.message || "fallback"` pattern always falls
/// through to the fallback in that case, hiding the actual backend
/// error from the user.
export function formatInvokeError(e: unknown, fallback: string): string {
  if (typeof e === "string") return e || fallback;
  if (e instanceof Error) return e.message || fallback;
  if (e && typeof e === "object" && "message" in e) {
    const msg = (e as { message?: unknown }).message;
    if (typeof msg === "string" && msg) return msg;
  }
  return fallback;
}
