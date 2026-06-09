//! synnotes-core — the vault domain logic shared by the SynNotes desktop app
//! (Tauri) and the `zen` CLI. Pure Rust with no GUI/Tauri dependency, so it
//! builds as a fully-static `x86_64-unknown-linux-musl` binary for the CLI.
//!
//! Ported from the ZenNotes Electron `main/` process (vault file I/O, metadata
//! extraction, search, tasks, comments, templates, settings, demo tour).

pub mod search;
pub mod types;
pub mod vault;
