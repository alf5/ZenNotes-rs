//! Filesystem watcher — port of apps/desktop/src/main/watcher.ts (chokidar →
//! `notify-debouncer-full`). Watches the vault root, classifies each change
//! into a `VaultChangeEvent` (with the same vault-settings / comments /
//! agent-edits / templates / database / folder / content scoping), and emits
//! `vault://change` to the frontend.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

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
const TEMPLATES_PREFIX: &str = ".zennotes/templates/";
const AGENT_EDITS_PREFIX: &str = ".zennotes/agent-edits/";
const AGENT_EDITS_SUFFIX: &str = ".json";
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
/// pre-mapped "add"/"change"/"unlink" string. `is_dir` selects the
/// directory route (chokidar's addDir/unlinkDir → scope "folder").
pub fn classify_change(root: &Path, abs: &Path, kind: &str, is_dir: bool) -> Option<VaultChangeEvent> {
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

    // An assistant edited a note through MCP, or a review moved on: the
    // note's banner and the review list follow the record. A record being
    // written is a `.tmp` beside it and stays out until renamed into place.
    if let Some(note_path) = agent_edit_note_path(&rel) {
        return Some(VaultChangeEvent {
            kind: kind.to_string(),
            folder: folder_for_relative_path(&note_path).unwrap_or_else(|| "inbox".into()),
            path: note_path,
            scope: Some("agent-edits".to_string()),
        });
    }

    // A template is not a note: its own scope has the renderer re-list
    // templates instead of the note tree (#723).
    if is_template_path(&rel) {
        return Some(VaultChangeEvent {
            kind: kind.to_string(),
            path: rel,
            folder: "inbox".to_string(),
            scope: Some("templates".to_string()),
        });
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
        if crate::vault::layout::ATTACHMENTS_DIRS.contains(&top) {
            Some("inbox".to_string())
        } else {
            None
        }
    })?;

    // Directory create/remove (upstream addDir/unlinkDir): an empty folder
    // produces no file event, so surface it explicitly with scope "folder".
    // Dir modify events have no chokidar equivalent — drop them.
    if is_dir {
        if kind == "change" {
            return None;
        }
        return Some(VaultChangeEvent {
            kind: kind.to_string(),
            path: rel,
            folder,
            scope: Some("folder".to_string()),
        });
    }

    // Database files (data.csv / schema.json / legacy loose .csv + sidecar)
    // normalize to the canonical data.csv path with scope "database"
    // (upstream watcher.ts:87). Record-page .md files return None here and
    // ride the normal note path.
    if let Some(csv_path) = database_csv_path_for(&rel) {
        return Some(VaultChangeEvent {
            kind: kind.to_string(),
            folder: folder_for_relative_path(&csv_path).unwrap_or(folder),
            path: csv_path,
            scope: Some("database".to_string()),
        });
    }

    Some(VaultChangeEvent {
        kind: kind.to_string(),
        path: rel,
        folder,
        scope: None,
    })
}

/// Port of watcher.ts `agentEditNotePath`: the note an agent edit record
/// belongs to.
fn agent_edit_note_path(rel: &str) -> Option<String> {
    let note = rel.strip_prefix(AGENT_EDITS_PREFIX)?.strip_suffix(AGENT_EDITS_SUFFIX)?;
    (!note.is_empty()).then(|| note.to_string())
}

/// Port of watcher.ts `templatePath`: a `.md` file directly inside
/// `.zennotes/templates/`, the flat directory the template module serves.
/// Dotfiles and nested paths are not templates there either.
fn is_template_path(rel: &str) -> bool {
    let Some(name) = rel.strip_prefix(TEMPLATES_PREFIX) else {
        return false;
    };
    !name.is_empty()
        && !name.contains('/')
        && !name.starts_with('.')
        && name.to_lowercase().ends_with(".md")
}

/// Rust port of shared-domain `databaseCsvPathFor` (databases.ts:300): map any
/// database file to its canonical `data.csv` path, or None for non-database
/// paths (including record pages).
fn database_csv_path_for(rel: &str) -> Option<String> {
    let lower = rel.to_lowercase();
    let parent = rel.rfind('/').map(|i| &rel[..i]);
    let parent_is_form_dir =
        parent.is_some_and(|dir| dir.to_lowercase().ends_with(".base"));
    if lower.ends_with("/schema.json") && parent_is_form_dir {
        return Some(format!("{}/data.csv", parent.unwrap_or_default()));
    }
    if lower.ends_with("/data.csv") && parent_is_form_dir {
        return Some(rel.to_string());
    }
    // Legacy: a `<Name>.csv.base.json` sidecar maps to its `.csv`.
    if lower.ends_with(".csv.base.json") {
        return Some(rel[..rel.len() - ".base.json".len()].to_string());
    }
    // Legacy loose `.csv` (not inside a `.base` folder).
    if lower.ends_with(".csv")
        && !rel
            .split('/')
            .rev()
            .skip(1)
            .any(|component| component.to_lowercase().ends_with(".base"))
    {
        return Some(rel.to_string());
    }
    None
}

/// Whether the notify event kind itself identifies a directory.
fn event_kind_is_dir(kind: &EventKind) -> bool {
    use notify_debouncer_full::notify::event::{CreateKind, RemoveKind};
    matches!(
        kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    )
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

// ---- the vault's watch, kept alive ------------------------------------------
// Ports of what watcher.ts and window-vaults.ts learned in v2.80–v2.85:
//  - A watcher that runs out of watch handles (inotify's ENOSPC, EMFILE) stops
//    and tries again later, 30 s doubling up to 10 min, and the window re-reads
//    the vault once it is back, because nothing reported in between.
//  - A folder that cannot be watched at all is re-read instead (#1023): every
//    30 s and when a window gains focus, refreshing only if something moved.
//    Upstream does this for a Windows network share (fs.watch's UNKNOWN). Here
//    it also covers Linux, where notify's recursive watch gives up on the whole
//    vault over one unreadable folder (a `lost+found` at the top of a mounted
//    drive); chokidar skips such a folder (`ignorePermissionErrors`).
//  - A burst of root-level changes (folders copied in) has the seeded inbox
//    looked at once it settles (`scheduleSeededInboxSettle`, 400 ms).

/// Watch a config folder (themes, language packs) and the folders right
/// inside it. notify's recursive watch gives up on the whole folder over one
/// subfolder it cannot read; chokidar skips that one (upstream v2.81,
/// `ignorePermissionErrors`), so fall back to each readable folder one level
/// down, which is as deep as upstream looks (`depth: 1`).
pub fn watch_config_dir(debouncer: &mut VaultDebouncer, dir: &Path) -> Result<(), String> {
    if debouncer.watcher().watch(dir, RecursiveMode::Recursive).is_ok() {
        debouncer.cache().add_root(dir, RecursiveMode::Recursive);
        return Ok(());
    }
    debouncer
        .watcher()
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch {}: {e}", dir.display()))?;
    debouncer.cache().add_root(dir, RecursiveMode::NonRecursive);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if let Err(error) = debouncer.watcher().watch(&entry.path(), RecursiveMode::NonRecursive) {
                eprintln!("could not watch {}: {error}", entry.path().display());
            }
        }
    }
    Ok(())
}

/// The re-read every refresh asks for: whatever changed was never reported.
fn resync_event() -> VaultChangeEvent {
    VaultChangeEvent {
        kind: "change".into(),
        path: String::new(),
        folder: "inbox".into(),
        scope: Some("resync".into()),
    }
}

/// Port of `vaultChangeAffectsSettings`: what the layout inference reads.
fn change_affects_settings(ev: &VaultChangeEvent) -> bool {
    match ev.scope.as_deref() {
        Some("vault-settings") => true,
        Some("comments") | Some("agent-edits") | Some("database") => false,
        _ => !ev.path.contains('/'),
    }
}

const WATCH_RETRY_FIRST_DELAY: Duration = Duration::from_secs(30);
const WATCH_RETRY_MAX_DELAY: Duration = Duration::from_secs(10 * 60);
/// Each re-read stats every file, over the network for a share.
const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
/// Focus can arrive in bursts, and a re-read is not free.
const RESCAN_FOCUS_GAP: Duration = Duration::from_secs(5);
/// A copy arrives as a burst of root-level events; one look after it is enough.
const SEEDED_INBOX_SETTLE_DELAY: Duration = Duration::from_millis(400);

struct Rescan {
    fingerprint: Option<String>,
    last_ran: Option<Instant>,
    running: bool,
    again: bool,
}

struct WatchInner {
    app: AppHandle,
    root: PathBuf,
    stopped: AtomicBool,
    debouncer: Mutex<Option<VaultDebouncer>>,
    /// Set once this vault's folder refused native events.
    rescan: Mutex<Option<Rescan>>,
    /// Feeds the seeded-inbox settle; None for a temporary folder session.
    settle: Mutex<Option<mpsc::Sender<()>>>,
}

/// The active vault's watch. Dropping it stops native events, re-reads and
/// retries alike.
pub struct VaultWatch {
    inner: Arc<WatchInner>,
}

impl Drop for VaultWatch {
    fn drop(&mut self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.settle.lock().unwrap().take();
        let debouncer = self.inner.debouncer.lock().unwrap().take();
        drop(debouncer);
    }
}

impl VaultWatch {
    /// A window gained focus: re-read a vault that cannot report changes.
    /// Does nothing for one native events cover.
    pub fn rescan_now(&self) {
        WatchInner::request_rescan(&self.inner, false);
    }
}

enum StartError {
    /// Out of watch handles: worth trying again later.
    Exhausted(String),
    /// This folder cannot be watched natively.
    Unsupported(String),
}

fn is_exhaustion(error: &notify_debouncer_full::notify::Error) -> bool {
    use notify_debouncer_full::notify::ErrorKind;
    match &error.kind {
        ErrorKind::MaxFilesWatch => true,
        ErrorKind::Io(io) => matches!(io.raw_os_error(), Some(code) if is_exhaustion_code(code)),
        _ => false,
    }
}

#[cfg(unix)]
fn is_exhaustion_code(code: i32) -> bool {
    // EMFILE, ENFILE, ENOSPC (inotify watches).
    code == 24 || code == 23 || code == 28
}

#[cfg(not(unix))]
fn is_exhaustion_code(_code: i32) -> bool {
    false
}

impl WatchInner {
    fn emit(&self, change: VaultChangeEvent) {
        if !self.stopped.load(Ordering::SeqCst) {
            let _ = self.app.emit(VAULT_CHANGE_EVENT, change);
        }
    }

    fn start_native(this: &Arc<Self>) -> Result<(), StartError> {
        let watch_root = this.root.clone();
        let weak = Arc::downgrade(this);
        let mut debouncer = new_debouncer(Duration::from_millis(120), None, move |result: DebounceEventResult| {
            let Some(inner) = weak.upgrade() else { return };
            match result {
                Ok(events) => {
                    for event in events {
                        // The kernel dropped events (inotify's queue overflowed):
                        // only a full re-read knows what changed.
                        if event.need_rescan() {
                            inner.emit(resync_event());
                            continue;
                        }
                        let Some(kind) = kind_str(&event.kind) else { continue };
                        let kind_is_dir = event_kind_is_dir(&event.kind);
                        for path in &event.paths {
                            // Removed paths can't be stat'ed; rely on the event
                            // kind there (notify's rename-from of a dir stays
                            // undetected; the child file events still refresh).
                            let is_dir = kind_is_dir || path.is_dir();
                            if let Some(change) = classify_change(&watch_root, path, kind, is_dir) {
                                if change_affects_settings(&change) {
                                    if let Some(tx) = inner.settle.lock().unwrap().as_ref() {
                                        let _ = tx.send(());
                                    }
                                }
                                inner.emit(change);
                            }
                        }
                    }
                }
                Err(errors) => {
                    // Running out of handles is the one error that ends the
                    // watch: every folder after it fails the same way. It is
                    // handled off this thread, which the debouncer owns.
                    if errors.iter().any(is_exhaustion) {
                        eprintln!("vault watcher ran out of watch handles; live updates have stopped");
                        std::thread::spawn(move || WatchInner::exhausted(&inner, WATCH_RETRY_FIRST_DELAY));
                    } else {
                        for error in errors {
                            eprintln!("vault watcher could not watch a path: {error}");
                        }
                    }
                }
            }
        })
        .map_err(|error| {
            if is_exhaustion(&error) {
                StartError::Exhausted(error.to_string())
            } else {
                StartError::Unsupported(error.to_string())
            }
        })?;
        if let Err(error) = debouncer.watcher().watch(&this.root, RecursiveMode::Recursive) {
            return Err(if is_exhaustion(&error) {
                StartError::Exhausted(error.to_string())
            } else {
                StartError::Unsupported(error.to_string())
            });
        }
        debouncer.cache().add_root(&this.root, RecursiveMode::Recursive);
        if this.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        *this.debouncer.lock().unwrap() = Some(debouncer);
        Ok(())
    }

    fn start(this: &Arc<Self>) {
        match Self::start_native(this) {
            Ok(()) => {}
            Err(StartError::Exhausted(message)) => {
                eprintln!("vault watcher ran out of watch handles: {message}");
                let inner = this.clone();
                std::thread::spawn(move || WatchInner::retry(&inner, WATCH_RETRY_FIRST_DELAY));
            }
            Err(StartError::Unsupported(message)) => {
                eprintln!("vault folder cannot report changes ({message}); re-reading it instead");
                Self::start_rescanning(this);
            }
        }
    }

    /// The watcher stopped itself after running out of handles mid-session.
    fn exhausted(this: &Arc<Self>, delay: Duration) {
        let debouncer = this.debouncer.lock().unwrap().take();
        if debouncer.is_none() {
            return; // Another error from the same flood already handled it.
        }
        drop(debouncer);
        Self::retry(this, delay);
    }

    /// Wait, then watch again, longer each time it fails. The windows re-read
    /// the vault once a watcher is back.
    fn retry(this: &Arc<Self>, first_delay: Duration) {
        let mut delay = first_delay;
        loop {
            eprintln!("vault watcher for {} stopped; trying again in {} s", this.root.display(), delay.as_secs());
            let until = Instant::now() + delay;
            while Instant::now() < until {
                if this.stopped.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            match Self::start_native(this) {
                Ok(()) => {
                    this.emit(resync_event());
                    return;
                }
                Err(StartError::Exhausted(_)) => delay = (delay * 2).min(WATCH_RETRY_MAX_DELAY),
                Err(StartError::Unsupported(message)) => {
                    eprintln!("vault folder cannot report changes ({message}); re-reading it instead");
                    Self::start_rescanning(this);
                    return;
                }
            }
        }
    }

    fn start_rescanning(this: &Arc<Self>) {
        *this.rescan.lock().unwrap() = Some(Rescan { fingerprint: None, last_ran: None, running: false, again: false });
        // Nothing reported what changed between opening the vault and now.
        this.emit(resync_event());
        Self::request_rescan(this, true);
        let inner = this.clone();
        std::thread::spawn(move || loop {
            let until = Instant::now() + RESCAN_INTERVAL;
            while Instant::now() < until {
                if inner.stopped.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            // The timer skips a turn rather than queue one: a read slower than
            // the interval would otherwise never stop reading.
            let busy = inner.rescan.lock().unwrap().as_ref().is_some_and(|r| r.running);
            if !busy {
                WatchInner::request_rescan(&inner, true);
            }
        });
    }

    /// Port of `rescanNow`.
    fn request_rescan(this: &Arc<Self>, force: bool) {
        {
            let mut guard = this.rescan.lock().unwrap();
            let Some(rescan) = guard.as_mut() else { return };
            if rescan.running {
                // The read under way may already have passed what this caller
                // is asking about, so a forced request reads once more after it.
                if force {
                    rescan.again = true;
                }
                return;
            }
            if !force && rescan.last_ran.is_some_and(|at| at.elapsed() < RESCAN_FOCUS_GAP) {
                return;
            }
            rescan.last_ran = Some(Instant::now());
            rescan.running = true;
        }
        let inner = this.clone();
        std::thread::spawn(move || loop {
            if let Some(rescan) = inner.rescan.lock().unwrap().as_mut() {
                rescan.again = false;
            }
            let next = vault_fingerprint(&inner.root);
            if inner.stopped.load(Ordering::SeqCst) {
                return;
            }
            let mut guard = inner.rescan.lock().unwrap();
            let Some(rescan) = guard.as_mut() else { return };
            if let Some(next) = next {
                let before = rescan.fingerprint.replace(next.clone());
                if before.is_some_and(|before| before != next) {
                    drop(guard);
                    inner.emit(resync_event());
                    guard = inner.rescan.lock().unwrap();
                }
            }
            let Some(rescan) = guard.as_mut() else { return };
            if !rescan.again {
                rescan.running = false;
                return;
            }
        });
    }
}

/// Port of vault-fingerprint.ts: every visible file's path, size and modified
/// time, reduced to one hash. None when the vault could not be read, because
/// a share that dropped mid-walk looks like a vault that lost its notes.
pub fn vault_fingerprint(root: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::ErrorKind;
    fn skipped(error: &std::io::Error) -> bool {
        matches!(error.kind(), ErrorKind::PermissionDenied | ErrorKind::NotFound)
    }
    fn stamp(meta: &std::fs::Metadata) -> String {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}\0{modified}", meta.len())
    }
    fn walk(root: &Path, dir: &Path, lines: &mut Vec<String>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || is_atomic_write_temp_name(&name) {
                continue;
            }
            let full = entry.path();
            let rel = to_posix(&full.strip_prefix(root).unwrap_or(&full).to_string_lossy());
            let result = (|| -> std::io::Result<()> {
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    lines.push(format!("{rel}/"));
                    walk(root, &full, lines)
                } else if file_type.is_file() || file_type.is_symlink() {
                    lines.push(format!("{rel}\0{}", stamp(&std::fs::metadata(&full)?)));
                    Ok(())
                } else {
                    Ok(())
                }
            })();
            if let Err(error) = result {
                if !skipped(&error) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    let mut lines = Vec::new();
    walk(root, root, &mut lines).ok()?;
    let settings = root.join(VAULT_SETTINGS_RELATIVE_PATH);
    if let Ok(meta) = std::fs::metadata(&settings) {
        lines.push(format!("{VAULT_SETTINGS_RELATIVE_PATH}\0{}", stamp(&meta)));
    }
    lines.sort();
    Some(format!("{:x}", Sha256::digest(lines.join("\n").as_bytes())))
}

/// Port of `isAtomicWriteTempPath`: `<name>.<pid>.<13+ digit stamp>.tmp`.
fn is_atomic_write_temp_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".tmp") else { return false };
    let mut parts = stem.rsplitn(3, '.');
    let stamp = parts.next().unwrap_or("");
    let pid = parts.next().unwrap_or("");
    parts.next().is_some()
        && stamp.len() >= 13
        && stamp.bytes().all(|b| b.is_ascii_digit())
        && !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
}

/// Start watching `root`, emitting `vault://change` events. Never fails: a
/// vault whose folder cannot be watched is re-read instead, and one that ran
/// out of watch handles is tried again later. `settle_inbox` is false for a
/// temporary folder session, which the app never changes on its own.
pub fn spawn(app: AppHandle, root: PathBuf, settle_inbox: bool) -> VaultWatch {
    let settle = settle_inbox.then(|| {
        let (tx, rx) = mpsc::channel::<()>();
        let settle_root = root.clone();
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(SEEDED_INBOX_SETTLE_DELAY) {
                        Ok(()) => continue,
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                crate::vault::seeded_inbox::settle_seeded_inbox(&settle_root);
            }
        });
        tx
    });
    let inner = Arc::new(WatchInner {
        app,
        root,
        stopped: AtomicBool::new(false),
        debouncer: Mutex::new(None),
        rescan: Mutex::new(None),
        settle: Mutex::new(settle),
    });
    WatchInner::start(&inner);
    VaultWatch { inner }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_affecting_changes_are_root_level_or_vault_json() {
        let ev = |path: &str, scope: Option<&str>| VaultChangeEvent {
            kind: "add".into(),
            path: path.into(),
            folder: "inbox".into(),
            scope: scope.map(str::to_string),
        };
        assert!(change_affects_settings(&ev(".zennotes/vault.json", Some("vault-settings"))));
        assert!(change_affects_settings(&ev("Projects", Some("folder"))));
        assert!(change_affects_settings(&ev("Plan.md", None)));
        assert!(!change_affects_settings(&ev("Projects/Plan.md", None)));
        assert!(!change_affects_settings(&ev("Plan.md", Some("comments"))));
    }

    #[test]
    fn atomic_write_temp_names_need_an_epoch_stamp() {
        assert!(is_atomic_write_temp_name("Plan.md.4242.1731400000000123.tmp"));
        assert!(!is_atomic_write_temp_name("notes.2024.01.tmp"));
        assert!(!is_atomic_write_temp_name("Plan.md"));
    }

    #[test]
    fn fingerprint_moves_only_when_the_vault_does() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
        std::fs::write(dir.path().join("inbox/A.md"), "a").unwrap();
        let first = vault_fingerprint(dir.path()).unwrap();
        assert_eq!(vault_fingerprint(dir.path()).unwrap(), first);
        // Hidden entries and temp files are not the vault's.
        std::fs::write(dir.path().join(".DS_Store"), "x").unwrap();
        std::fs::write(dir.path().join("inbox/A.md.1.1731400000000123.tmp"), "x").unwrap();
        assert_eq!(vault_fingerprint(dir.path()).unwrap(), first);
        std::fs::create_dir_all(dir.path().join("Projects")).unwrap();
        assert_ne!(vault_fingerprint(dir.path()).unwrap(), first);
        assert!(vault_fingerprint(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn classifies_vault_settings() {
        let root = Path::new("/v");
        let ev = classify_change(root, Path::new("/v/.zennotes/vault.json"), "change", false).unwrap();
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
            false,
        )
        .unwrap();
        assert_eq!(ev.scope.as_deref(), Some("comments"));
        assert_eq!(ev.path, "inbox/Note.md");
        assert_eq!(ev.folder, "inbox");
    }

    #[test]
    fn classifies_agent_edit_records_by_their_note() {
        let root = Path::new("/v");
        let ev = classify_change(
            root,
            Path::new("/v/.zennotes/agent-edits/inbox/Plan.md.json"),
            "add",
            false,
        )
        .unwrap();
        assert_eq!(ev.scope.as_deref(), Some("agent-edits"));
        assert_eq!(ev.path, "inbox/Plan.md");
        // The temp file a record is written through stays out.
        assert!(classify_change(
            root,
            Path::new("/v/.zennotes/agent-edits/inbox/Plan.md.json.0f3a.tmp"),
            "add",
            false
        )
        .is_none());
    }

    #[test]
    fn classifies_templates_but_not_nested_or_dotfiles() {
        let root = Path::new("/v");
        let ev = classify_change(root, Path::new("/v/.zennotes/templates/Meeting.md"), "change", false)
            .unwrap();
        assert_eq!(ev.scope.as_deref(), Some("templates"));
        assert_eq!(ev.path, ".zennotes/templates/Meeting.md");
        assert!(classify_change(root, Path::new("/v/.zennotes/templates/.Draft.md"), "add", false).is_none());
        assert!(classify_change(root, Path::new("/v/.zennotes/templates/a/b.md"), "add", false).is_none());
        assert!(classify_change(root, Path::new("/v/.zennotes/templates/notes.txt"), "add", false).is_none());
    }

    #[test]
    fn classifies_database_files_normalized_to_data_csv() {
        let root = Path::new("/v");
        let schema =
            classify_change(root, Path::new("/v/inbox/Books.base/schema.json"), "change", false)
                .unwrap();
        assert_eq!(schema.scope.as_deref(), Some("database"));
        assert_eq!(schema.path, "inbox/Books.base/data.csv");

        let data = classify_change(root, Path::new("/v/inbox/Books.base/data.csv"), "add", false)
            .unwrap();
        assert_eq!(data.scope.as_deref(), Some("database"));
        assert_eq!(data.path, "inbox/Books.base/data.csv");

        // Legacy loose csv + sidecar.
        assert_eq!(
            database_csv_path_for("inbox/Old.csv.base.json").as_deref(),
            Some("inbox/Old.csv")
        );
        assert_eq!(database_csv_path_for("inbox/Old.csv").as_deref(), Some("inbox/Old.csv"));

        // Record pages ride the normal note path.
        let page =
            classify_change(root, Path::new("/v/inbox/Books.base/Dune.md"), "change", false)
                .unwrap();
        assert_eq!(page.scope, None);
        assert_eq!(page.path, "inbox/Books.base/Dune.md");
    }

    #[test]
    fn classifies_directory_events_with_folder_scope() {
        let root = Path::new("/v");
        let added = classify_change(root, Path::new("/v/inbox/Projects"), "add", true).unwrap();
        assert_eq!(added.scope.as_deref(), Some("folder"));
        assert_eq!(added.path, "inbox/Projects");

        let removed = classify_change(root, Path::new("/v/inbox/Projects"), "unlink", true).unwrap();
        assert_eq!(removed.scope.as_deref(), Some("folder"));

        // Dir modify events have no chokidar equivalent.
        assert!(classify_change(root, Path::new("/v/inbox/Projects"), "change", true).is_none());
    }

    #[test]
    fn classifies_content_and_attachments() {
        let root = Path::new("/v");
        let note = classify_change(root, Path::new("/v/inbox/A.md"), "change", false).unwrap();
        assert_eq!(note.scope, None);
        assert_eq!(note.folder, "inbox");

        let asset = classify_change(root, Path::new("/v/attachements/pic.png"), "add", false).unwrap();
        assert_eq!(asset.folder, "inbox");
    }

    #[test]
    fn ignores_internal_and_dot_and_node_modules() {
        let root = Path::new("/v");
        assert!(classify_change(root, Path::new("/v/.zennotes/note-meta-cache-v1.json"), "change", false).is_none());
        assert!(classify_change(root, Path::new("/v/inbox/.DS_Store"), "add", false).is_none());
        assert!(classify_change(root, Path::new("/v/node_modules/x/y.md"), "add", false).is_none());
    }
}
