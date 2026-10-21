//! Undo history kept between launches (#793, Vim's `undofile`) — port of
//! apps/desktop/src/main/undo-history-store.ts.
//!
//! The files sit under the app's own data folder, never in the vault: undo
//! data is a machine-local convenience, it holds fragments of text the user
//! deleted, and inside `.zennotes/` it would sync to other machines and land
//! in the history of a git-backed vault. One file per note, named by hashes,
//! so a note path can never steer a write outside this folder:
//!
//!   <app data>/undo-history/<sha256(vault)[:16]>/<sha256(note path)[:32]>.json
//!
//! The content is opaque here; the renderer decides what is in it and whether
//! it still fits the note. This module only bounds how much disk it takes.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};

use crate::state::AppState;

const UNDO_HISTORY_DIR: &str = "undo-history";
/// One note's history. Beyond this the renderer trims its oldest steps first.
const MAX_UNDO_HISTORY_BYTES: u64 = 2 * 1024 * 1024;
/// Notes remembered per vault; the ones written longest ago go first.
const MAX_UNDO_HISTORY_FILES: usize = 400;
/// A history nobody came back to for this long is not coming back.
const MAX_UNDO_HISTORY_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);
const MAX_NOTE_PATH_LENGTH: usize = 4096;

fn digest(value: &str, length: usize) -> String {
    let hash = Sha256::digest(value.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    hex[..length].to_string()
}

fn vault_dir(base: &Path, vault_identity: &str) -> PathBuf {
    base.join(UNDO_HISTORY_DIR).join(digest(vault_identity, 16))
}

/// None for anything that is not a plausible note path.
pub fn undo_history_file(base: &Path, vault_identity: &str, note_path: &str) -> Option<PathBuf> {
    // Counted in UTF-16 units, like `String.length` upstream.
    let units = note_path.encode_utf16().count();
    if units == 0 || units > MAX_NOTE_PATH_LENGTH || note_path.contains('\0') {
        return None;
    }
    Some(vault_dir(base, vault_identity).join(format!("{}.json", digest(note_path, 32))))
}

pub fn read_undo_history(base: &Path, vault_identity: &str, note_path: &str) -> Result<Option<String>, String> {
    let Some(file) = undo_history_file(base, vault_identity, note_path) else {
        return Ok(None);
    };
    // One handle for the size check and the read: checking the path and then
    // reading it again would let the file be swapped in between.
    let mut handle = match fs::File::open(&file) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read failed: {error}")),
    };
    let size = handle.metadata().map_err(|e| format!("stat failed: {e}"))?.len();
    if size > MAX_UNDO_HISTORY_BYTES {
        return Ok(None);
    }
    let mut text = String::new();
    handle.read_to_string(&mut text).map_err(|e| format!("read failed: {e}"))?;
    Ok(Some(text))
}

/// `json == None` forgets the note's history. Oversized input is dropped.
pub fn write_undo_history(
    base: &Path,
    vault_identity: &str,
    note_path: &str,
    json: Option<&str>,
) -> Result<(), String> {
    let Some(file) = undo_history_file(base, vault_identity, note_path) else {
        return Ok(());
    };
    let Some(json) = json else {
        return match fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!("remove failed: {error}")),
            _ => Ok(()),
        };
    };
    if json.len() as u64 > MAX_UNDO_HISTORY_BYTES {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    // Written beside the target and renamed over it: a quit in the middle of
    // a write must not leave half a file for the next launch to parse.
    let partial = PathBuf::from(format!("{}.{}.tmp", file.display(), std::process::id()));
    fs::write(&partial, json).map_err(|e| format!("write failed: {e}"))?;
    fs::rename(&partial, &file).map_err(|e| format!("write failed: {e}"))
}

/// Everything, for every vault: what turning the setting off promises.
pub fn clear_undo_histories(base: &Path) -> Result<(), String> {
    match fs::remove_dir_all(base.join(UNDO_HISTORY_DIR)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!("remove failed: {error}")),
        _ => Ok(()),
    }
}

/// Keep the folder bounded: drop histories older than the age limit, then the
/// oldest ones beyond the per-vault count, and any stray partial writes.
pub fn prune_undo_histories(base: &Path, now: SystemTime, max_files: usize) {
    let Ok(vaults) = fs::read_dir(base.join(UNDO_HISTORY_DIR)) else {
        return;
    };
    for vault in vaults.flatten() {
        let dir = vault.path();
        let Ok(names) = fs::read_dir(&dir) else { continue };
        let mut kept: Vec<(PathBuf, SystemTime)> = Vec::new();
        for entry in names.flatten() {
            let file = entry.path();
            // Raced with a write or a clear: the next launch sees the result.
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
            let expired = now.duration_since(modified).unwrap_or_default() > MAX_UNDO_HISTORY_AGE;
            let is_json = entry.file_name().to_string_lossy().ends_with(".json");
            if !is_json || expired {
                let _ = fs::remove_file(&file);
            } else {
                kept.push((file, modified));
            }
        }
        kept.sort_by(|a, b| b.1.cmp(&a.1));
        for (file, _) in kept.iter().skip(max_files) {
            let _ = fs::remove_file(file);
        }
        if kept.is_empty() {
            let _ = fs::remove_dir(&dir);
        }
    }
}

/// Run once per launch, off the boot path.
pub fn spawn_prune(app: &AppHandle) {
    if let Ok(base) = app.path().app_data_dir() {
        std::thread::spawn(move || prune_undo_histories(&base, SystemTime::now(), MAX_UNDO_HISTORY_FILES));
    }
}

fn base_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| format!("Could not resolve app data dir: {e}"))
}

/// Keyed by the vault of the app's state, never by anything the renderer
/// names: the note path only ever feeds a hash.
fn vault_identity(state: &AppState) -> Result<String, String> {
    state
        .current()
        .map(|v| v.root)
        .ok_or_else(|| "No vault is currently open".to_string())
}

#[tauri::command]
pub fn undo_history_read(app: AppHandle, state: State<AppState>, note_path: String) -> Result<Option<String>, String> {
    read_undo_history(&base_dir(&app)?, &vault_identity(&state)?, &note_path)
}

#[tauri::command]
pub fn undo_history_write(
    app: AppHandle,
    state: State<AppState>,
    note_path: String,
    json: Option<String>,
) -> Result<(), String> {
    write_undo_history(&base_dir(&app)?, &vault_identity(&state)?, &note_path, json.as_deref())
}

#[tauri::command]
pub fn undo_history_clear(app: AppHandle) -> Result<(), String> {
    clear_undo_histories(&base_dir(&app)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_forgets_and_clears() {
        let base = tempfile::tempdir().unwrap();
        let b = base.path();
        assert_eq!(read_undo_history(b, "/v", "inbox/A.md").unwrap(), None);
        write_undo_history(b, "/v", "inbox/A.md", Some("{\"steps\":1}")).unwrap();
        assert_eq!(read_undo_history(b, "/v", "inbox/A.md").unwrap().as_deref(), Some("{\"steps\":1}"));
        // Another vault with the same note path is another history.
        assert_eq!(read_undo_history(b, "/w", "inbox/A.md").unwrap(), None);
        write_undo_history(b, "/v", "inbox/A.md", None).unwrap();
        assert_eq!(read_undo_history(b, "/v", "inbox/A.md").unwrap(), None);
        write_undo_history(b, "/v", "inbox/B.md", Some("x")).unwrap();
        clear_undo_histories(b).unwrap();
        assert!(!b.join(UNDO_HISTORY_DIR).exists());
    }

    #[test]
    fn names_files_by_hash_and_refuses_implausible_paths() {
        let base = Path::new("/data");
        let file = undo_history_file(base, "/v", "../../etc/passwd").unwrap();
        assert!(file.starts_with("/data/undo-history"));
        assert_eq!(file.file_name().unwrap().len(), 32 + ".json".len());
        assert_eq!(undo_history_file(base, "/v", ""), None);
        assert_eq!(undo_history_file(base, "/v", "a\0b"), None);
        assert_eq!(undo_history_file(base, "/v", &"x".repeat(4097)), None);
    }

    #[test]
    fn drops_oversized_writes() {
        let base = tempfile::tempdir().unwrap();
        let huge = "x".repeat(MAX_UNDO_HISTORY_BYTES as usize + 1);
        write_undo_history(base.path(), "/v", "inbox/A.md", Some(&huge)).unwrap();
        assert_eq!(read_undo_history(base.path(), "/v", "inbox/A.md").unwrap(), None);
    }

    #[test]
    fn prune_keeps_the_newest_and_drops_strays() {
        let base = tempfile::tempdir().unwrap();
        let b = base.path();
        for i in 0..5 {
            write_undo_history(b, "/v", &format!("inbox/{i}.md"), Some("x")).unwrap();
        }
        let dir = vault_dir(b, "/v");
        fs::write(dir.join("stray.tmp"), "partial").unwrap();
        prune_undo_histories(b, SystemTime::now(), 3);
        let left: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(left.len(), 3);
        // Far in the future, everything has expired and the folder goes too.
        prune_undo_histories(b, SystemTime::now() + MAX_UNDO_HISTORY_AGE * 2, 3);
        assert!(!dir.exists());
    }
}
