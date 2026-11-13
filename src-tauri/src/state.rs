//! Shared application state: the open workspace (a local vault, or a remote
//! session), the local vault's filesystem watcher, and per-window bits.
//! Every window follows the one open workspace.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::ipc::types::VaultInfo;
use crate::remote::RemoteSession;
use crate::watcher::VaultDebouncer;

#[derive(Default)]
pub struct AppState {
    /// The open LOCAL vault. Always `None` while a remote session is on, so
    /// every command that works on the local disk refuses then.
    pub current_vault: Mutex<Option<VaultInfo>>,
    /// The live remote workspace, when there is one.
    remote: Mutex<Option<Arc<RemoteSession>>>,
    /// Why the configured remote workspace failed to connect at boot.
    remote_boot_error: Mutex<Option<String>>,
    /// Bumped whenever a local vault takes over, so a connection that was
    /// still handshaking does not then replace it.
    workspace_generation: AtomicU64,
    /// Serializes connects and boots: several windows asking for the current
    /// vault at launch must not each open a connection.
    pub workspace_lock: Mutex<()>,
    /// Active vault watcher; dropping it stops watching.
    pub watcher: Mutex<Option<VaultDebouncer>>,
    /// External (out-of-vault) markdown files bound to standalone editor
    /// windows, keyed by window label.
    pub external_files: Mutex<HashMap<String, PathBuf>>,
    /// Notes queued to open in the main window before the renderer signalled
    /// ready (deep links / file-open at launch). Flushed on `app_renderer_ready`.
    pub pending_open_notes: Mutex<Vec<String>>,
    /// Whether the main renderer has mounted and subscribed to open-note events.
    pub renderer_ready: Mutex<bool>,
}

impl AppState {
    pub fn current(&self) -> Option<VaultInfo> {
        self.current_vault.lock().unwrap().clone()
    }

    pub fn set_current(&self, vault: Option<VaultInfo>) {
        *self.current_vault.lock().unwrap() = vault;
    }

    /// Absolute root path of the open LOCAL vault, if any. `None` while a
    /// remote session is on: that is what fences the local disk off.
    pub fn current_root(&self) -> Option<PathBuf> {
        self.current().map(|v| PathBuf::from(v.root))
    }

    pub fn remote_session(&self) -> Option<Arc<RemoteSession>> {
        self.remote.lock().unwrap().clone()
    }

    /// Install or end the remote session. Ending it stops its change feed.
    pub fn set_remote_session(&self, session: Option<Arc<RemoteSession>>) {
        let previous = std::mem::replace(&mut *self.remote.lock().unwrap(), session);
        drop(previous);
    }

    /// A local vault takes over the workspace: end any remote session, and
    /// make sure a connection still in flight does not replace this vault.
    /// Returns whether a remote session was ended.
    pub fn take_over_from_remote(&self) -> bool {
        self.workspace_generation.fetch_add(1, Ordering::SeqCst);
        *self.remote_boot_error.lock().unwrap() = None;
        let previous = self.remote.lock().unwrap().take();
        previous.is_some()
    }

    pub fn workspace_generation(&self) -> u64 {
        self.workspace_generation.load(Ordering::SeqCst)
    }

    pub fn remote_boot_error(&self) -> Option<String> {
        self.remote_boot_error.lock().unwrap().clone()
    }

    pub fn set_remote_boot_error(&self, error: Option<String>) {
        *self.remote_boot_error.lock().unwrap() = error;
    }

    /// The workspace's vault, local or remote.
    pub fn workspace_vault(&self) -> Option<VaultInfo> {
        self.current().or_else(|| self.remote_session().and_then(|s| s.vault_info()))
    }

    /// Replace the active watcher (the previous one, if any, is dropped/stopped).
    pub fn set_watcher(&self, watcher: Option<VaultDebouncer>) {
        *self.watcher.lock().unwrap() = watcher;
    }

    pub fn set_external_file(&self, label: &str, path: PathBuf) {
        self.external_files.lock().unwrap().insert(label.to_string(), path);
    }

    pub fn external_file(&self, label: &str) -> Option<PathBuf> {
        self.external_files.lock().unwrap().get(label).cloned()
    }

    pub fn is_renderer_ready(&self) -> bool {
        *self.renderer_ready.lock().unwrap()
    }

    pub fn set_renderer_ready(&self, ready: bool) {
        *self.renderer_ready.lock().unwrap() = ready;
    }

    pub fn queue_open_note(&self, rel: String) {
        self.pending_open_notes.lock().unwrap().push(rel);
    }

    pub fn drain_pending_open_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_open_notes.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::RemoteSession;

    fn local() -> VaultInfo {
        VaultInfo { root: "/home/me/Notes".into(), name: "Notes".into(), temporary: None }
    }

    #[test]
    fn a_remote_session_leaves_no_local_root_to_touch() {
        let state = AppState::default();
        state.set_current(None);
        state.set_remote_session(Some(RemoteSession::for_test(
            "https://zen.example",
            serde_json::json!({ "root": "/My vaults/Research", "name": "Research" }),
        )));
        assert_eq!(state.current_root(), None);
        let err = crate::ipc::vault_cmds::require_root(&state).unwrap_err();
        assert_eq!(err, crate::ipc::vault_cmds::LOCAL_ONLY_WHILE_REMOTE);
        // The workspace still has a vault: the server's.
        assert_eq!(state.workspace_vault().map(|v| v.root).as_deref(), Some("/My vaults/Research"));
    }

    #[test]
    fn a_local_vault_ends_the_session_and_outdates_a_connect_in_flight() {
        let state = AppState::default();
        state.set_remote_session(Some(RemoteSession::for_test("https://zen.example", serde_json::Value::Null)));
        state.set_remote_boot_error(Some("down".into()));
        let generation = state.workspace_generation();
        assert!(state.take_over_from_remote());
        state.set_current(Some(local()));
        assert!(state.remote_session().is_none());
        assert_eq!(state.remote_boot_error(), None);
        assert_ne!(state.workspace_generation(), generation);
        assert_eq!(state.current_root().as_deref(), Some(std::path::Path::new("/home/me/Notes")));
        assert!(!state.take_over_from_remote());
    }

    /// Every place that opens a local vault first ends any remote session, so
    /// the two are never on at once. A new call site that forgets fails here.
    #[test]
    fn every_local_open_ends_the_remote_session_first() {
        // Opening one: `set_current(Some(` outside these, which only rename
        // the vault already open.
        const RENAMES_ONLY: &[&str] = &["sync_local_vault_name"];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") || path.ends_with("state.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (at, _) in text.match_indices("set_current(Some(") {
                    let before = &text[..at];
                    let fn_at = before.rfind("fn ").unwrap();
                    let name: String = before[fn_at + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if RENAMES_ONLY.contains(&name.as_str()) {
                        continue;
                    }
                    if !before[fn_at..].contains("take_over_from_remote()") {
                        offenders.push(format!("{}: {name}", path.display()));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "local vault opened without ending the remote session: {offenders:?}");
    }
}
