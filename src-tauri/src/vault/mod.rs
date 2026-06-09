//! Vault domain logic ported from apps/desktop/src/main/vault.ts. Pure
//! functions take a `root: &Path` and never import `tauri`, so they're
//! exercised directly by `cargo test`.

pub mod assets;
pub mod comments;
pub mod config;
pub mod crud;
pub mod demo_tour;
pub mod folders;
pub mod layout;
pub mod listing;
pub mod metadata;
pub mod notes;
pub mod settings;
pub mod tasks;
pub mod templates;
