//! Shared application state. For the single-window milestones this holds the
//! currently open vault + its filesystem watcher; multi-window vault routing
//! arrives in M11.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::ipc::types::VaultInfo;
use crate::watcher::VaultDebouncer;

#[derive(Default)]
pub struct AppState {
    pub current_vault: Mutex<Option<VaultInfo>>,
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

    /// Absolute root path of the open vault, if any.
    pub fn current_root(&self) -> Option<PathBuf> {
        self.current().map(|v| PathBuf::from(v.root))
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
