//! User-installed TextMate code languages — the file layer under
//! src/bridge/code-languages.ts. Ports the fs half of upstream
//! `apps/desktop/src/main/custom-code-languages.ts`; grammar parsing and
//! manifest validation stay in the webview (vendored shared-domain functions),
//! so what counts as a legal fence tag or a reserved alias has one definition.
//!
//! Layout (a sibling of config.toml, see app_config::config_dir):
//!   languages/README.md
//!   languages/<id>/{manifest.json, grammar.tmLanguage.json}
//!
//! The watcher emits a content-free ping (`code-languages://changed`); the
//! frontend re-scans and fans a fresh list out to subscribers.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify_debouncer_full::notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::app_config::config_dir;
use crate::watcher::VaultDebouncer;

const CHANGED_EVENT: &str = "code-languages://changed";
const WATCH_DEBOUNCE_MS: u64 = 200;

const MANIFEST_NAME: &str = "manifest.json";
const GRAMMAR_NAME: &str = "grammar.tmLanguage.json";

/// Upstream's CUSTOM_CODE_LANGUAGE_MAX_BYTES. Enforced here as well as in the
/// webview so a hand-dropped folder cannot make the renderer read 200 MB of
/// JSON before shared-domain gets a chance to refuse it.
const MAX_GRAMMAR_BYTES: u64 = 2 * 1024 * 1024;

const README: &str = "# ZenNotes custom code languages

Each installed language has its own folder containing a manifest and a
self-contained TextMate JSON grammar. Use Settings → Editor → Languages to
install, edit, enable, disable, or remove languages safely.

```
languages/
  gleam/
    manifest.json
    grammar.tmLanguage.json
```
";

/// Keeps the dir debouncer alive for the app's lifetime.
pub struct CodeLanguageWatchers(#[allow(dead_code)] pub Mutex<Vec<VaultDebouncer>>);

fn languages_dir() -> PathBuf {
    config_dir().join("languages")
}

/// The same shape shared-domain's `isValidCodeFenceTag` accepts, which is also
/// what makes an id safe as a single path segment: no separators, no dots, so
/// no traversal and no `..`.
fn is_safe_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else { return false };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if id.len() > 64 {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '+' | '#' | '-'))
}

/// One language folder, unparsed: the frontend runs `parseTextMateGrammar` and
/// `validateCustomCodeLanguageManifest`, and builds the error stub when either
/// file is missing or invalid.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLanguageEntry {
    pub id: String,
    pub manifest: Option<String>,
    pub grammar: Option<String>,
}

fn read_capped(path: &Path, max: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    fs::read_to_string(path).ok()
}

pub fn ensure_languages_dir() -> Result<PathBuf, String> {
    let dir = languages_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let readme = dir.join("README.md");
    // create_new instead of exists-then-write: the filesystem answers "already
    // there" atomically, so nothing lands in the gap between a check and the
    // write and an existing README is never overwritten.
    if let Ok(mut file) = fs::OpenOptions::new().write(true).create_new(true).open(readme) {
        use std::io::Write;
        let _ = file.write_all(README.as_bytes());
    }
    Ok(dir)
}

#[tauri::command]
pub fn code_languages_scan() -> Vec<RawLanguageEntry> {
    let dir = languages_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<RawLanguageEntry> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') || !is_safe_id(&id) {
            continue;
        }
        let folder = dir.join(&id);
        out.push(RawLanguageEntry {
            manifest: read_capped(&folder.join(MANIFEST_NAME), MAX_GRAMMAR_BYTES),
            grammar: read_capped(&folder.join(GRAMMAR_NAME), MAX_GRAMMAR_BYTES),
            id,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Write both files of a language folder. The manifest is written last, so a
/// crash between the two leaves a folder the scan reports as broken rather than
/// one whose manifest promises a grammar that is not there.
#[tauri::command]
pub fn code_languages_write(id: String, manifest: String, grammar: String) -> Result<(), String> {
    if !is_safe_id(&id) {
        return Err(format!("Not a valid language id: {id}"));
    }
    if grammar.len() as u64 > MAX_GRAMMAR_BYTES {
        return Err("Grammar files must be 2 MB or smaller.".into());
    }
    let dir = ensure_languages_dir()?;
    let folder = dir.join(&id);
    fs::create_dir_all(&folder).map_err(|e| format!("mkdir failed: {e}"))?;
    write_atomic(&folder.join(GRAMMAR_NAME), &grammar)?;
    write_atomic(&folder.join(MANIFEST_NAME), &manifest)
}

/// Rewrite only the manifest — the enable/rename/alias path, which must never
/// touch the grammar the user installed.
#[tauri::command]
pub fn code_languages_write_manifest(id: String, manifest: String) -> Result<(), String> {
    if !is_safe_id(&id) {
        return Err(format!("Not a valid language id: {id}"));
    }
    let folder = languages_dir().join(&id);
    if !folder.is_dir() {
        return Err(format!("No installed language with the fence tag \"{id}\"."));
    }
    write_atomic(&folder.join(MANIFEST_NAME), &manifest)
}

#[tauri::command]
pub fn code_languages_delete(id: String) -> Result<(), String> {
    if !is_safe_id(&id) {
        return Ok(());
    }
    let dir = languages_dir();
    let folder = dir.join(&id);
    // `is_safe_id` already makes an escape impossible; this is the same
    // belt-and-braces parent check the themes deletion does.
    if folder.parent() != Some(dir.as_path()) {
        return Ok(());
    }
    let _ = fs::remove_dir_all(folder);
    Ok(())
}

#[tauri::command]
pub fn code_languages_reveal(app: AppHandle, id: Option<String>) -> Result<(), String> {
    let dir = ensure_languages_dir()?;
    let mut target = dir.clone();
    if let Some(id) = id.as_deref().filter(|i| is_safe_id(i)) {
        let folder = dir.join(id);
        if folder.parent() == Some(dir.as_path()) && folder.is_dir() {
            target = folder;
        }
    }
    crate::os::reveal_file_path(&app, &target.to_string_lossy())
}

/// Watch the languages dir recursively (a grammar edit is a change to a file
/// one level down) and ping the frontend, which re-scans.
pub fn spawn_watcher(app: AppHandle) -> Result<Vec<VaultDebouncer>, String> {
    let dir = ensure_languages_dir()?;
    let handle = app.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(WATCH_DEBOUNCE_MS),
        None,
        move |result: DebounceEventResult| {
            if result.is_ok() {
                let _ = handle.emit(CHANGED_EVENT, ());
            }
        },
    )
    .map_err(|e| format!("Failed to create watcher: {e}"))?;
    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch {}: {e}", dir.display()))?;
    debouncer.cache().add_root(&dir, RecursiveMode::Recursive);
    Ok(vec![debouncer])
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return fs::write(path, text).map_err(|e| format!("write failed: {e}"));
    };
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        std::process::id()
    ));
    fs::write(&tmp, text).map_err(|e| format!("write failed: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_single_safe_path_segments() {
        assert!(is_safe_id("gleam"));
        assert!(is_safe_id("c++"));
        assert!(is_safe_id("f#"));
        assert!(is_safe_id("2fa_x-y"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("-leading"));
        assert!(!is_safe_id("Upper"));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id("a\\b"));
        assert!(!is_safe_id("a.b"));
        assert!(!is_safe_id(&"x".repeat(65)));
    }

    #[test]
    fn read_capped_refuses_an_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.json");
        fs::write(&small, "{}").unwrap();
        assert_eq!(read_capped(&small, MAX_GRAMMAR_BYTES).as_deref(), Some("{}"));

        let big = dir.path().join("big.json");
        fs::write(&big, "x".repeat(64)).unwrap();
        assert!(read_capped(&big, 8).is_none());

        assert!(read_capped(&dir.path().join("missing.json"), MAX_GRAMMAR_BYTES).is_none());
        // A directory is not a grammar.
        assert!(read_capped(dir.path(), MAX_GRAMMAR_BYTES).is_none());
    }

    #[test]
    fn atomic_write_replaces_the_file_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("manifest.json");
        write_atomic(&target, "{\"a\":1}").unwrap();
        write_atomic(&target, "{\"a\":2}").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "{\"a\":2}");
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
