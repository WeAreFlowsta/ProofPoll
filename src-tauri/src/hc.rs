//! One face over two Holochain client lines.
//!
//! During the 0.6 -> 0.7 transition the app runs two conductors: 0.6.1 for
//! DNA v1.0-v1.3 and 0.7.1 for v1.4. A `Record` from one line does not
//! decode with the other line's types (the 0.7 action model changed the wire
//! shape), so everything above this module works with [`RecordView`], our own
//! line-free view of a record, and sends zome calls through [`AppClient`],
//! which knows which line it speaks.
//!
//! What crosses the boundary unchanged: our own entry structs (msgpack, the
//! same bytes on both lines) and hashes (39 raw bytes; the `u…` base64url
//! spelling is the same on both lines).

use holochain_types::prelude::ExternIO as ExternIO06;
use holochain_types_07::prelude::ExternIO as ExternIO07;

/// Which Holochain line a conductor speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Line {
    /// Holochain 0.6.1: DNA v1.0 to v1.3.
    V06,
    /// Holochain 0.7.1: DNA v1.4 and later.
    V07,
}

/// An app websocket on one of the two lines.
pub enum AppClient {
    V06(holochain_client::AppWebsocket),
    V07(holochain_client_07::AppWebsocket),
}

impl AppClient {
    pub fn line(&self) -> Line {
        match self {
            AppClient::V06(_) => Line::V06,
            AppClient::V07(_) => Line::V07,
        }
    }

    /// Call a zome function. `payload` is the msgpack of the input (what
    /// `ExternIO::encode` produces); the result is the msgpack of the output.
    pub async fn call_zome(
        &self,
        role: &str,
        zome: &str,
        fn_name: &str,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        match self {
            AppClient::V06(c) => {
                use holochain_client::ZomeCallTarget;
                use holochain_types::prelude::{FunctionName, RoleName, ZomeName};
                c.call_zome(
                    ZomeCallTarget::RoleName(RoleName::from(role)),
                    ZomeName::from(zome),
                    FunctionName::from(fn_name),
                    ExternIO06::from(payload),
                )
                .await
                .map(|io| io.into_vec())
                .map_err(|e| format!("{}", e))
            }
            AppClient::V07(c) => {
                use holochain_client_07::ZomeCallTarget;
                use holochain_types_07::prelude::{FunctionName, RoleName, ZomeName};
                c.call_zome(
                    ZomeCallTarget::RoleName(RoleName::from(role)),
                    ZomeName::from(zome),
                    FunctionName::from(fn_name),
                    ExternIO07::from(payload),
                )
                .await
                .map(|io| io.into_vec())
                .map_err(|e| format!("{}", e))
            }
        }
    }
}

/// Encode a zome input the way `ExternIO::encode` does (msgpack, named fields).
pub fn encode<T: serde::Serialize + std::fmt::Debug>(value: &T) -> Result<Vec<u8>, String> {
    ExternIO06::encode(value)
        .map(|io| io.into_vec())
        .map_err(|e| e.to_string())
}

/// Decode a zome output that is one of our own (line-free) types.
pub fn decode<T: serde::de::DeserializeOwned + std::fmt::Debug>(bytes: &[u8]) -> Result<T, String> {
    ExternIO06::from(bytes.to_vec())
        .decode()
        .map_err(|e| format!("Failed to decode zome output: {}", e))
}

/// What the app needs from a record, whichever line produced it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RecordView {
    /// The action hash, `u…` base64url (the same spelling on both lines).
    pub action_hash: String,
    /// The author's agent key, `u…` base64url.
    pub author: String,
    /// Action timestamp, microseconds since the epoch.
    pub timestamp_us: i64,
    /// The app entry's msgpack bytes, when the record carries an entry.
    pub entry: Option<Vec<u8>>,
    /// Which line the record came from.
    pub line: Line,
}

impl RecordView {
    /// Decode the entry as one of our own entry structs.
    pub fn entry_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        let bytes = self.entry.as_ref().ok_or("Record has no entry")?;
        rmp_serde::from_slice(bytes).map_err(|e| format!("Failed to decode entry: {}", e))
    }
}

/// Decode a zome output of `Vec<Record>` from the given line.
pub fn records(line: Line, bytes: &[u8]) -> Result<Vec<RecordView>, String> {
    match line {
        Line::V06 => {
            let recs: Vec<holochain_types::prelude::Record> = decode(bytes)?;
            Ok(recs.iter().map(view_06).collect())
        }
        Line::V07 => {
            let recs: Vec<holochain_types_07::prelude::Record> = ExternIO07::from(bytes.to_vec())
                .decode()
                .map_err(|e| format!("Failed to decode records: {}", e))?;
            Ok(recs.iter().map(view_07).collect())
        }
    }
}

/// Decode a zome output of `Option<Record>` from the given line.
pub fn record_opt(line: Line, bytes: &[u8]) -> Result<Option<RecordView>, String> {
    match line {
        Line::V06 => {
            let rec: Option<holochain_types::prelude::Record> = decode(bytes)?;
            Ok(rec.as_ref().map(view_06))
        }
        Line::V07 => {
            let rec: Option<holochain_types_07::prelude::Record> = ExternIO07::from(bytes.to_vec())
                .decode()
                .map_err(|e| format!("Failed to decode record: {}", e))?;
            Ok(rec.as_ref().map(view_07))
        }
    }
}

fn view_06(r: &holochain_types::prelude::Record) -> RecordView {
    RecordView {
        action_hash: r.action_address().to_string(),
        author: r.action().author().to_string(),
        timestamp_us: r.action().timestamp().as_micros(),
        entry: r
            .entry()
            .as_option()
            .and_then(|e| e.as_app_entry())
            .map(|a| a.as_ref().bytes().to_vec()),
        line: Line::V06,
    }
}

fn view_07(r: &holochain_types_07::prelude::Record) -> RecordView {
    RecordView {
        action_hash: r.action_address().to_string(),
        author: r.action().author().to_string(),
        timestamp_us: r.action().timestamp().as_micros(),
        entry: r
            .entry()
            .as_option()
            .and_then(|e| e.as_app_entry())
            .map(|a| a.as_ref().bytes().to_vec()),
        line: Line::V07,
    }
}

/// Parse a `u…` hash string into its 39 raw bytes (either line, same spelling).
pub fn hash_bytes(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let b64 = s.strip_prefix('u').ok_or("Hash must start with 'u'")?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|e| format!("Invalid base64 in hash: {}", e))?;
    if raw.len() != 39 {
        return Err(format!("Hash wrong length: {} (expected 39)", raw.len()));
    }
    Ok(raw)
}

/// An `ActionHash` (0.6 type; the msgpack bytes are the same on both lines) from its string.
pub fn action_hash(s: &str) -> Result<holochain_types::prelude::ActionHash, String> {
    Ok(holochain_types::prelude::ActionHash::from_raw_39(hash_bytes(s)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_strings_round_trip_through_raw_bytes() {
        let h = holochain_types::prelude::ActionHash::from_raw_32(vec![7u8; 32]);
        let s = h.to_string();
        assert!(s.starts_with("uhCkk"));
        assert_eq!(action_hash(&s).unwrap(), h);
        // the 0.7 type spells the same bytes the same way
        let h7 = holochain_types_07::prelude::ActionHash::from_raw_39(hash_bytes(&s).unwrap());
        assert_eq!(h7.to_string(), s);
    }

    #[test]
    fn our_structs_encode_identically_for_both_lines() {
        #[derive(serde::Serialize, Debug)]
        struct In { title: String, n: u32 }
        let a = encode(&In { title: "x".into(), n: 2 }).unwrap();
        let b = ExternIO07::encode(&In { title: "x".into(), n: 2 }).unwrap().into_vec();
        assert_eq!(a, b);
    }
}
