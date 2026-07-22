//! Portable app config (`config.toml`) — the file layer under
//! src/bridge/portable-config.ts, which owns the TOML format (upstream's
//! annotated serializer runs in the webview). This module only resolves the
//! XDG-style directory, reads/writes the file atomically, and watches it for
//! external edits, emitting the fresh raw text as `config://file-changed`.
//! The own-write loop-guard lives with the serializer in the frontend.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tauri::{AppHandle, Emitter};

use crate::watcher::VaultDebouncer;

pub const CONFIG_FILE_NAME: &str = "config.toml";
const FILE_CHANGED_EVENT: &str = "config://file-changed";
const WATCH_DEBOUNCE_MS: u64 = 150;

/// Keeps the config-file debouncer alive for the app's lifetime (never read,
/// only dropped on exit).
pub struct PortableConfigWatcher(#[allow(dead_code)] pub Mutex<Option<VaultDebouncer>>);

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    return std::env::var_os("HOME").map(PathBuf::from);
}

fn env_path(name: &str) -> Option<PathBuf> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Resolve the config directory, honoring overrides in priority order:
/// `$ZENNOTES_CONFIG_DIR` → `$XDG_CONFIG_HOME/zennotes` → platform default.
/// Deliberately `~/.config/zennotes` on macOS too (upstream #203 — dotfile
/// syncable), NOT the Tauri app-config dir (which holds the app's own
/// machine-local `zennotes-rs.config.json`).
pub fn config_dir() -> PathBuf {
    if let Some(explicit) = env_path("ZENNOTES_CONFIG_DIR") {
        return explicit;
    }
    if let Some(xdg) = env_path("XDG_CONFIG_HOME") {
        return xdg.join("zennotes");
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = env_path("APPDATA") {
            return appdata.join("zennotes");
        }
        return home_dir()
            .unwrap_or_default()
            .join("AppData")
            .join("Roaming")
            .join("zennotes");
    }
    #[cfg(not(windows))]
    home_dir().unwrap_or_default().join(".config").join("zennotes")
}

pub fn config_file() -> PathBuf {
    config_dir().join(CONFIG_FILE_NAME)
}

/// Atomic write: temp file in the same dir + rename.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "config path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    let tmp = parent.join(format!("{CONFIG_FILE_NAME}.{}.tmp", std::process::id()));
    fs::write(&tmp, text).map_err(|e| format!("write failed: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename failed: {e}"))
}

#[tauri::command]
pub fn config_file_path() -> String {
    config_file().to_string_lossy().to_string()
}

#[tauri::command]
pub fn config_file_read() -> Result<Option<String>, String> {
    match fs::read_to_string(config_file()) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("config read failed: {e}")),
    }
}

#[tauri::command]
pub fn config_file_write(text: String) -> Result<(), String> {
    write_atomic(&config_file(), &text)
}

/// Watch the config dir (non-recursive) for `config.toml` changes and push
/// the fresh text to every window. Deletions are intentionally not forwarded
/// (a deleted config shouldn't wipe live settings; the next save recreates
/// it). The frontend's loop-guard filters out our own writes.
pub fn spawn_watcher(app: AppHandle) -> Result<VaultDebouncer, String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let mut debouncer = new_debouncer(
        Duration::from_millis(WATCH_DEBOUNCE_MS),
        None,
        move |result: DebounceEventResult| {
            let Ok(events) = result else { return };
            let touched_config = events.iter().any(|event| {
                !matches!(event.kind, EventKind::Remove(_))
                    && event
                        .paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|n| n == CONFIG_FILE_NAME))
            });
            if !touched_config {
                return;
            }
            if let Ok(text) = fs::read_to_string(config_file()) {
                let _ = app.emit(FILE_CHANGED_EVENT, text);
            }
        },
    )
    .map_err(|e| format!("Failed to create config watcher: {e}"))?;

    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch config dir: {e}"))?;
    debouncer.cache().add_root(&dir, RecursiveMode::NonRecursive);
    Ok(debouncer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(CONFIG_FILE_NAME);
        write_atomic(&file, "config_version = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "config_version = 1\n");
        write_atomic(&file, "config_version = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "config_version = 2\n");
        assert!(!dir
            .path()
            .read_dir()
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
    }
}
