//! Imports from Obsidian, Logseq and Notion into the open local vault: the
//! host's half. Upstream runs all of it in its main process
//! (apps/desktop/src/main/{obsidian,logseq,notion}-import{,-run}.ts and
//! notion-sign-in.ts). Here the run itself, and the conversion it drives
//! (vendored shared-domain), stays in the webview (src/bridge/imports/), and
//! Rust keeps only what a webview can't do or shouldn't hold:
//!
//! - the folder dialog and the folder it picked, which the webview may read
//!   but never name (`session.rs`), and the state of each run, so every window
//!   sees the same progress and a reload finds it again;
//! - every file operation, on virtual paths that each name one of four places
//!   (`paths.rs`, `files.rs`): the picked folder (read only), the run's
//!   staging folder under the app's data folder, and the vault the run began
//!   in, where an import only creates and appends: it never overwrites or
//!   deletes a thing there;
//! - requests to Notion, sent only to its two hosts with the credential
//!   attached here (`notion_http.rs`), and Sign in with Notion, whose tokens
//!   live in the OS keychain and never reach the webview (`notion_auth.rs`).

pub mod commands;
mod files;
mod notion_auth;
mod notion_http;
mod paths;
mod session;
