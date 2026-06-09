//! Filesystem watcher — port of apps/desktop/src/main/watcher.ts (chokidar →
//! `notify-debouncer-full`). Watches the vault root, classifies each change
//! into a `VaultChangeEvent` (with the same vault-settings / comments / content
//! scoping), and emits `vault://change` to the frontend.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer, notify::RecommendedWatcher, DebounceEventResult, Debouncer, FileIdMap,
};
use tauri::{AppHandle, Emitter};

use crate::ipc::types::VaultChangeEvent;
use crate::vault::config::resolve_path;
use crate::vault::notes::{folder_for_relative_path, to_posix};

pub type VaultDebouncer = Debouncer<RecommendedWatcher, FileIdMap>;

const VAULT_SETTINGS_RELATIVE_PATH: &str = ".zennotes/vault.json";
const NOTE_COMMENTS_PREFIX: &str = ".zennotes/comments/";
const NOTE_COMMENTS_SUFFIX: &str = ".comments.json";
const ATTACHMENTS_DIRS: [&str; 2] = ["attachements", "_assets"];
const VAULT_CHANGE_EVENT: &str = "vault://change";

fn relative_vault_path(root: &Path, abs: &Path) -> String {
    let root_abs = resolve_path(&root.to_string_lossy());
    let abs_str = resolve_path(&abs.to_string_lossy());
    let rel = abs_str
        .strip_prefix(&format!("{root_abs}{}", std::path::MAIN_SEPARATOR))
        .unwrap_or(&abs_str);
    to_posix(rel)
}

/// Routing port of watcher.ts's handler: maps a changed path to (rel path,
/// folder, scope), or `None` when the change should be ignored. `kind` is the
/// pre-mapped "add"/"change"/"unlink" string.
pub fn classify_change(root: &Path, abs: &Path, kind: &str) -> Option<VaultChangeEvent> {
    let rel = relative_vault_path(root, abs);

    if rel == VAULT_SETTINGS_RELATIVE_PATH {
        return Some(VaultChangeEvent {
            kind: kind.to_string(),
            path: VAULT_SETTINGS_RELATIVE_PATH.to_string(),
            folder: "inbox".to_string(),
            scope: Some("vault-settings".to_string()),
        });
    }

    if let Some(stripped) = rel.strip_prefix(NOTE_COMMENTS_PREFIX) {
        if let Some(note_path) = stripped.strip_suffix(NOTE_COMMENTS_SUFFIX) {
            return Some(VaultChangeEvent {
                kind: kind.to_string(),
                path: note_path.to_string(),
                folder: folder_for_relative_path(note_path).unwrap_or_else(|| "inbox".into()),
                scope: Some("comments".to_string()),
            });
        }
    }

    // Prune dot-prefixed components (e.g. other .zennotes internals) and
    // node_modules — chokidar ignored these from the watch entirely.
    for component in rel.split('/') {
        if component.starts_with('.') || component == "node_modules" {
            return None;
        }
    }

    let folder = folder_for_relative_path(&rel).or_else(|| {
        let top = rel.split('/').next().unwrap_or("");
        if ATTACHMENTS_DIRS.contains(&top) {
            Some("inbox".to_string())
        } else {
            None
        }
    })?;

    Some(VaultChangeEvent {
        kind: kind.to_string(),
        path: rel,
        folder,
        scope: None,
    })
}

/// Map a notify event kind to chokidar-style add/change/unlink strings.
fn kind_str(kind: &EventKind) -> Option<&'static str> {
    use notify_debouncer_full::notify::event::{ModifyKind, RenameMode};
    match kind {
        EventKind::Create(_) => Some("add"),
        EventKind::Remove(_) => Some("unlink"),
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Some("unlink"),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Some("add"),
        EventKind::Modify(_) => Some("change"),
        _ => None,
    }
}

/// Start watching `root`, emitting `vault://change` events. The returned
/// debouncer must be kept alive (stored in AppState); dropping it stops the
/// watch.
pub fn spawn(app: AppHandle, root: PathBuf) -> Result<VaultDebouncer, String> {
    let watch_root = root.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(120),
        None,
        move |result: DebounceEventResult| {
            let Ok(events) = result else { return };
            for event in events {
                let Some(kind) = kind_str(&event.kind) else { continue };
                for path in &event.paths {
                    if let Some(change) = classify_change(&watch_root, path, kind) {
                        let _ = app.emit(VAULT_CHANGE_EVENT, change);
                    }
                }
            }
        },
    )
    .map_err(|e| format!("Failed to create watcher: {e}"))?;

    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch vault: {e}"))?;
    debouncer.cache().add_root(&root, RecursiveMode::Recursive);
    Ok(debouncer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_vault_settings() {
        let root = Path::new("/v");
        let ev = classify_change(root, Path::new("/v/.zennotes/vault.json"), "change").unwrap();
        assert_eq!(ev.scope.as_deref(), Some("vault-settings"));
        assert_eq!(ev.path, ".zennotes/vault.json");
        assert_eq!(ev.folder, "inbox");
    }

    #[test]
    fn classifies_comments() {
        let root = Path::new("/v");
        let ev = classify_change(
            root,
            Path::new("/v/.zennotes/comments/inbox/Note.md.comments.json"),
            "add",
        )
        .unwrap();
        assert_eq!(ev.scope.as_deref(), Some("comments"));
        assert_eq!(ev.path, "inbox/Note.md");
        assert_eq!(ev.folder, "inbox");
    }

    #[test]
    fn classifies_content_and_attachments() {
        let root = Path::new("/v");
        let note = classify_change(root, Path::new("/v/inbox/A.md"), "change").unwrap();
        assert_eq!(note.scope, None);
        assert_eq!(note.folder, "inbox");

        let asset = classify_change(root, Path::new("/v/attachements/pic.png"), "add").unwrap();
        assert_eq!(asset.folder, "inbox");
    }

    #[test]
    fn ignores_internal_and_dot_and_node_modules() {
        let root = Path::new("/v");
        assert!(classify_change(root, Path::new("/v/.zennotes/note-meta-cache-v1.json"), "change").is_none());
        assert!(classify_change(root, Path::new("/v/inbox/.DS_Store"), "add").is_none());
        assert!(classify_change(root, Path::new("/v/node_modules/x/y.md"), "add").is_none());
    }
}
