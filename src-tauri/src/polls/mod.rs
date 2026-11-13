//! Everything that is about POLLS: the zome's entry types, the commands the
//! page calls for polls, votes, flags, drafts and vote reasons, and the test
//! harness's poll shapes.
//!
//! ## For forking developers
//!
//! This folder is the app. The rest of `src-tauri/src` is plumbing that any
//! Holochain desktop app keeps as it is: the two lines and their conductors
//! (`conductor.rs`, on flowsta-hc-lines), the key store, Flowsta sign-in and
//! identity linking, Vault backups and restore, the updater, the profiles.
//! To build your own app, replace what is in this folder with your own entry
//! types and commands, register them in `lib.rs`, and change the three
//! places outside it that read poll records: the backup's record
//! classification and the export in `commands.rs` (`classify_app_entry`,
//! `get_export_data`), and the move between DNA versions (`carry_move.rs`,
//! `migration.rs`).

mod commands;
mod flags;
#[cfg_attr(not(feature = "harness"), allow(dead_code, unused_imports))]
mod harness;
mod private;
mod types;

pub use commands::*;
pub use flags::*;
#[allow(unused_imports)]
pub(crate) use harness::*;
pub use private::*;
pub use types::*;
