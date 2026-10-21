//! A note's app-owned sidecars, and moving a note (or a folder) together with
//! them. Port of apps/desktop/src/main/{note-sidecars,note-creation-metadata,
//! agent-edit-files}.ts (the file layer; parsing an agent edit record stays
//! in the webview, through vendored `@shared/agent-edits`).
//!
//! Comments live at `.zennotes/comments/<note>.comments.json`, the creation
//! date at `.zennotes/note-metadata/<note>.metadata.json`, and an assistant's
//! unreviewed edit at `.zennotes/agent-edits/<note>.json`. All three follow
//! the note through a rename, a move and a delete, so the vault stays
//! interchangeable with upstream ZenNotes opening the same folder.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::vault::atomic::rename_with_retry;
use crate::vault::comments::comments_path;
use crate::vault::config::resolve_path;
use crate::vault::layout::INTERNAL_VAULT_DIR;
use crate::vault::notes::{created_at_ms, to_posix};

const NOTE_METADATA_DIR: &str = "note-metadata";
const NOTE_METADATA_SUFFIX: &str = ".metadata.json";
const AGENT_EDITS_DIR: &str = "agent-edits";
pub const AGENT_EDITS_SUFFIX: &str = ".json";
const FOLDER_STATE_UNCERTAIN: &str =
    "FOLDER_STATE_UNCERTAIN: Folder change could not be rolled back; reload the vault before editing";

fn not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn lexical(path: &Path) -> PathBuf {
    PathBuf::from(resolve_path(&path.to_string_lossy()))
}

/// `base` + `rel`, refusing anything that lands outside `base` (or on it).
fn inside(base: &Path, rel: &str, what: &str) -> Result<PathBuf, String> {
    let base = lexical(base);
    let target = lexical(&base.join(rel));
    if target == base || !target.starts_with(&base) {
        return Err(format!("Path escapes {what}: {rel}"));
    }
    Ok(target)
}

/// The vault-relative POSIX path of an absolute path under `root`.
pub fn rel_of(root: &Path, abs: &Path) -> String {
    let root = lexical(root);
    let abs = lexical(abs);
    to_posix(&abs.strip_prefix(&root).unwrap_or(&abs).to_string_lossy())
}

// ---- file identity ---------------------------------------------------------

/// Port of same-file.ts: what tells a case-only rename (the destination is
/// the file itself, on a case-insensitive disk) from a rename onto another
/// file. Follows symlinks, like `fs.stat`.
#[derive(Debug, PartialEq, Eq)]
pub struct FileIdentity(u64, u64, PathBuf);

pub fn file_identity(abs: &Path) -> std::io::Result<FileIdentity> {
    let meta = fs::metadata(abs)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = &meta;
        Ok(FileIdentity(meta.dev(), meta.ino(), PathBuf::new()))
    }
    #[cfg(not(unix))]
    {
        // Volume serial + file index are unstable APIs in std; the
        // canonical path names the same file on the same volume.
        let _ = meta;
        Ok(FileIdentity(0, 0, fs::canonicalize(abs)?))
    }
}

pub fn path_exists(abs: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(abs) {
        Ok(_) => Ok(true),
        Err(error) if not_found(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

// ---- creation dates (note-creation-metadata.ts) ----------------------------

pub fn note_metadata_root(root: &Path) -> PathBuf {
    lexical(&root.join(INTERNAL_VAULT_DIR).join(NOTE_METADATA_DIR))
}

/// Port of `noteMetadataPath`. Metadata is app-owned: a sidecar link must
/// never redirect a save outside the vault, even when the Markdown itself is
/// intentionally symlinked, so any symlink on the way is refused.
pub fn note_metadata_path(root: &Path, rel: &str, directory: bool) -> Result<PathBuf, String> {
    let suffix = if directory { "" } else { NOTE_METADATA_SUFFIX };
    let target = inside(&note_metadata_root(root), &format!("{rel}{suffix}"), "note metadata")?;
    let root_abs = lexical(root);
    let mut current = root_abs.clone();
    for part in target.strip_prefix(&root_abs).unwrap_or(&target).components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Symlinked note metadata is not supported".into())
            }
            Ok(_) => {}
            Err(error) if not_found(&error) => break,
            Err(error) => return Err(format!("stat failed: {error}")),
        }
    }
    Ok(target)
}

fn read_creation_metadata(abs: &Path) -> Result<Option<i64>, String> {
    let raw = match fs::read_to_string(abs) {
        Ok(raw) => raw,
        Err(error) if not_found(&error) => return Ok(None),
        Err(error) => return Err(format!("read failed: {error}")),
    };
    let invalid = || format!("Invalid note creation metadata: {}", abs.display());
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|_| invalid())?;
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let created_at = value
        .get("createdAt")
        .and_then(serde_json::Value::as_f64)
        .filter(|n| n.fract() == 0.0 && n.abs() <= MAX_SAFE_INTEGER);
    match created_at {
        Some(n)
            if raw.encode_utf16().count() <= 4096
                && value.get("version").and_then(serde_json::Value::as_f64) == Some(1.0)
                && n > 0.0
                && n <= 8_640_000_000_000_000.0 =>
        {
            Ok(Some(n as i64))
        }
        _ => Err(invalid()),
    }
}

/// Port of `readNoteCreatedAt`. Corrupt optional metadata must not hide the
/// user's Markdown, so any failure falls back to the file's own date. Saves
/// are stricter, so they cannot silently discard a date that needs repair.
pub fn read_note_created_at(root: &Path, rel: &str, fallback: i64) -> i64 {
    note_metadata_path(root, rel, false)
        .ok()
        .and_then(|path| read_creation_metadata(&path).ok().flatten())
        .unwrap_or(fallback)
}

/// Port of `prepareNoteCreation`: before a note is replaced by temp file and
/// rename (which gives it a new inode and a new birth time), record the date
/// it was created, once.
pub fn prepare_note_creation(root: &Path, rel: &str) -> Result<(), String> {
    let posix = rel.replace('\\', "/");
    let lower = posix.to_lowercase();
    if posix.starts_with(".zennotes/") || !(lower.ends_with(".md") || lower.ends_with(".excalidraw")) {
        return Ok(());
    }
    let root_abs = lexical(root);
    let abs = lexical(&root_abs.join(&posix));
    if abs == root_abs || !abs.starts_with(&root_abs) {
        return Err("Path escapes vault".into());
    }
    let metadata = note_metadata_path(root, &posix, false)?;
    // Stat before reading metadata: a concurrent writer publishes the original
    // date before replacing the inode, so observing the new inode is safe.
    let previous = match fs::metadata(&abs) {
        Ok(meta) => meta,
        Err(error) if not_found(&error) => {
            return match fs::remove_file(&metadata) {
                Err(error) if !not_found(&error) => Err(format!("remove failed: {error}")),
                _ => Ok(()),
            }
        }
        Err(error) => return Err(format!("stat failed: {error}")),
    };
    if read_creation_metadata(&metadata)?.is_some() {
        return Ok(());
    }
    let modified = previous
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let created_at = created_at_ms(&previous, modified);
    let parent = metadata.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&parent).map_err(|e| format!("mkdir failed: {e}"))?;
    let temporary = PathBuf::from(format!(
        "{}.{}.{}{:06}.tmp",
        metadata.display(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        uuid::Uuid::new_v4().as_u128() % 1_000_000
    ));
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(previous.permissions().mode() & 0o777);
        }
        let mut file = options.open(&temporary)?;
        // Byte-identical to upstream's JSON.stringify({ version, createdAt }).
        file.write_all(format!("{{\"version\":1,\"createdAt\":{created_at}}}\n").as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &metadata)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| format!("write failed: {e}"))
}

/// Port of `removeNoteCreation`: a creation date with no note beside it
/// belongs to nobody.
pub fn remove_note_creation(root: &Path, rel: &str, directory: bool) -> Result<(), String> {
    let path = note_metadata_path(root, rel, directory)?;
    let result = if directory { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    match result {
        Err(error) if !not_found(&error) => Err(format!("remove failed: {error}")),
        _ => Ok(()),
    }
}

// ---- agent edit records (agent-edit-files.ts) ------------------------------

pub fn agent_edits_root(root: &Path) -> PathBuf {
    lexical(&root.join(INTERNAL_VAULT_DIR).join(AGENT_EDITS_DIR))
}

/// The record file for a vault-relative note path, refusing any escape.
pub fn agent_edit_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    inside(&agent_edits_root(root), &format!("{}{AGENT_EDITS_SUFFIX}", to_posix(rel)), "the vault")
}

/// The folder holding the records of every note under `folder_rel`.
pub fn agent_edit_folder_path(root: &Path, folder_rel: &str) -> Result<PathBuf, String> {
    inside(&agent_edits_root(root), &to_posix(folder_rel), "the vault")
}

/// Leave no empty folders behind, agent-edits itself included: a vault whose
/// changes have all been reviewed carries no trace of them.
fn prune_empty_agent_edit_dirs(root: &Path, from: &Path) {
    let stop = agent_edits_root(root);
    let mut dir = lexical(from);
    while dir == stop || dir.starts_with(&stop) {
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
}

fn remove_agent_edit_file(root: &Path, file: &Path) -> Result<(), String> {
    match fs::remove_file(file) {
        Err(error) if !not_found(&error) => return Err(format!("remove failed: {error}")),
        _ => {}
    }
    if let Some(parent) = file.parent() {
        prune_empty_agent_edit_dirs(root, parent);
    }
    Ok(())
}

/// Port of `removeAgentEditRecord`: a note's record, or with `directory`
/// every record under a folder. A deleted note has nothing left to review.
pub fn remove_agent_edit_record(root: &Path, rel: &str, directory: bool) -> Result<(), String> {
    if !directory {
        return remove_agent_edit_file(root, &agent_edit_path(root, rel)?);
    }
    let folder = agent_edit_folder_path(root, rel)?;
    match fs::remove_dir_all(&folder) {
        Err(error) if !not_found(&error) => return Err(format!("remove failed: {error}")),
        _ => {}
    }
    if let Some(parent) = folder.parent() {
        prune_empty_agent_edit_dirs(root, parent);
    }
    Ok(())
}

/// The raw text of a note's record, or None when there is none or it cannot
/// be read. The webview parses it with `parseAgentEditRecord`.
pub fn read_agent_edit_text(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(agent_edit_path(root, rel).ok()?).ok()
}

/// Write an already-serialized record (temp file, fsync, rename), or remove
/// it for None.
pub fn write_agent_edit_text(root: &Path, rel: &str, text: Option<&str>) -> Result<(), String> {
    let file = agent_edit_path(root, rel)?;
    let Some(text) = text else {
        return remove_agent_edit_file(root, &file);
    };
    let parent = file.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&parent).map_err(|e| format!("mkdir failed: {e}"))?;
    let temporary = PathBuf::from(format!("{}.{}.tmp", file.display(), uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut handle = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        handle.write_all(text.as_bytes())?;
        handle.sync_all()?;
        drop(handle);
        rename_with_retry(&temporary, &file)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| format!("write failed: {e}"))
}

/// Every record file in the vault as (note path, raw text). Unreadable files
/// are skipped; the webview parses, summarizes and sorts.
pub fn list_agent_edit_texts(root: &Path) -> Vec<(String, String)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let abs = entry.path();
            if kind.is_dir() {
                walk(base, &abs, out);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !kind.is_file() || !name.ends_with(AGENT_EDITS_SUFFIX) {
                continue;
            }
            let rel = to_posix(&abs.strip_prefix(base).unwrap_or(&abs).to_string_lossy());
            let note_path = rel[..rel.len() - AGENT_EDITS_SUFFIX.len()].to_string();
            if let Ok(text) = fs::read_to_string(&abs) {
                out.push((note_path, text));
            }
        }
    }
    let base = agent_edits_root(root);
    let mut out = Vec::new();
    walk(&base, &base, &mut out);
    out
}

// ---- moving notes with their sidecars (note-sidecars.ts) --------------------

/// The words a rename or move shows when an earlier note's comments sit on
/// the destination name.
pub fn leftover_comments_message(root: &Path, note_abs: &Path) -> String {
    let comments = comments_path(root, &rel_of(root, note_abs));
    let stem = note_abs
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    format!(
        "Comments from an earlier note named “{stem}” are still in {}. Move or delete that file to use this name.",
        rel_of(root, &comments)
    )
}

/// Port of `rebaseMovedLink`: after a rename from `from` to `to`, keep a
/// symlink pointing where it did. A link's relative text is read from the
/// link's own folder, so a link moved verbatim to another depth names a
/// different file, or none; the text is re-based on the target it named from
/// the old folder, which is also what makes a rename back land on the
/// original text again.
#[cfg(unix)]
pub fn rebase_moved_link(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = match fs::symlink_metadata(to) {
        Ok(meta) => meta,
        Err(error) if not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_symlink() {
        return Ok(());
    }
    let text = fs::read_link(to)?;
    if text.is_absolute() {
        return Ok(());
    }
    let (Some(from_dir), Some(to_dir)) = (from.parent(), to.parent()) else {
        return Ok(());
    };
    if from_dir == to_dir {
        return Ok(());
    }
    let next = relative_path(&lexical(to_dir), &lexical(&from_dir.join(&text)));
    if next == text {
        return Ok(());
    }
    // A new link beside it, renamed over: no moment without a link at `to`.
    let temporary = PathBuf::from(format!("{}_relink_tmp_{}", to.display(), uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(&next, &temporary)?;
    fs::rename(&temporary, to).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(not(unix))]
pub fn rebase_moved_link(_from: &Path, _to: &Path) -> std::io::Result<()> {
    // Symlinked notes are a POSIX affordance; Windows links need privileges
    // the app never asks for.
    Ok(())
}

/// Give the symlink at `abs` the text `text`, which must name the same file
/// the link already names: a spelling, never a new target. Checked here
/// rather than trusted, so an edited or synced run ledger cannot use undo to
/// plant a link to anywhere and then write through it.
pub fn respell_link(abs: &Path, text: &str) -> Result<(), String> {
    let current = fs::read_link(abs).map_err(|e| format!("not a link: {e}"))?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
    if lexical(&dir.join(&current)) != lexical(&dir.join(text)) {
        return Err("Refusing to point a link somewhere new".into());
    }
    #[cfg(unix)]
    {
        let temporary = PathBuf::from(format!("{}_relink_tmp_{}", abs.display(), uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(text, &temporary).map_err(|e| format!("symlink failed: {e}"))?;
        fs::rename(&temporary, abs).map_err(|e| {
            let _ = fs::remove_file(&temporary);
            format!("relink failed: {e}")
        })
    }
    #[cfg(not(unix))]
    {
        Err("Symlinked notes are not supported on this platform".into())
    }
}

/// `path.relative(from, to)` for two lexically normalized absolute paths.
#[cfg_attr(not(unix), allow(dead_code))]
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &to[common..] {
        out.push(part.as_os_str());
    }
    out
}

/// Rename a file or folder, two-step for a case-only change so a
/// case-insensitive disk sees it.
fn rename_directory(from: &Path, to: &Path) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    if from.to_string_lossy().to_lowercase() != to.to_string_lossy().to_lowercase() {
        fs::rename(from, to).map_err(|e| format!("rename failed: {e}"))?;
        return rebase_moved_link(from, to).map_err(|e| format!("relink failed: {e}"));
    }
    let temporary = PathBuf::from(format!("{}_rename_tmp_{}", from.display(), uuid::Uuid::new_v4()));
    fs::rename(from, &temporary).map_err(|e| format!("rename failed: {e}"))?;
    if let Err(error) = fs::rename(&temporary, to) {
        if fs::rename(&temporary, from).is_err() {
            return Err(FOLDER_STATE_UNCERTAIN.into());
        }
        return Err(format!("rename failed: {error}"));
    }
    Ok(())
}

/// Port of `relocateFolderTrees`: move content and its parallel sidecar trees
/// together, then persist settings; on any failure the moves made so far are
/// put back. A destination that already exists (and is not the source
/// itself, as in a case-only rename) refuses the whole change up front.
pub fn relocate_folder_trees(
    moves: Vec<(PathBuf, PathBuf)>,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut present = Vec::new();
    for (from, to) in moves {
        let source = match file_identity(&from) {
            Ok(id) => Some(id),
            Err(error) if not_found(&error) => None,
            Err(error) => return Err(format!("stat failed: {error}")),
        };
        match file_identity(&to) {
            Ok(target) if source.as_ref() != Some(&target) => {
                return Err("The destination folder or its comments already exist".into())
            }
            Ok(_) => {}
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(format!("stat failed: {error}")),
        }
        if source.is_some() {
            present.push((from, to));
        }
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut run = || -> Result<(), String> {
        for (from, to) in &present {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
            }
            rename_directory(from, to)?;
            moved.push((from.clone(), to.clone()));
        }
        Ok(())
    };
    let result = run().and_then(|()| persist());
    if let Err(error) = result {
        let mut rollback_failed = false;
        for (from, to) in moved.iter().rev() {
            if rename_directory(to, from).is_err() {
                rollback_failed = true;
            }
        }
        return Err(if rollback_failed { FOLDER_STATE_UNCERTAIN.into() } else { error });
    }
    Ok(())
}

/// Port of `relocateNote`: move one note with its comments, its creation date
/// and its edit record. A creation date or an edit record already waiting at
/// the destination with no note beside it belongs to nobody (the note that
/// owned it was moved or deleted outside ZenNotes) and is discarded, or every
/// rename onto that name would be refused for good (#839). Leftover comments
/// still refuse: taking over another note's discussion and deleting it are
/// both wrong, so the error names the file to move aside.
pub fn relocate_note(
    root: &Path,
    from_rel: &str,
    to_abs: &Path,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let to_rel = rel_of(root, to_abs);
    let to_comments = comments_path(root, &to_rel);
    if !path_exists(to_abs).map_err(|e| format!("stat failed: {e}"))? {
        if path_exists(&to_comments).map_err(|e| format!("stat failed: {e}"))? {
            return Err(leftover_comments_message(root, to_abs));
        }
        remove_note_creation(root, &to_rel, false)?;
        remove_agent_edit_record(root, &to_rel, false)?;
    }
    relocate_folder_trees(
        vec![
            (crate::vault::notes::resolve_safe(root, from_rel)?, to_abs.to_path_buf()),
            (comments_path(root, from_rel), to_comments),
            (note_metadata_path(root, from_rel, false)?, note_metadata_path(root, &to_rel, false)?),
            (agent_edit_path(root, from_rel)?, agent_edit_path(root, &to_rel)?),
        ],
        persist,
    )
}

/// A fresh, empty folder under `.zennotes/` to move doomed trees into, so a
/// delete can detach content and sidecars atomically before erasing them.
pub fn internal_temp_dir(root: &Path, prefix: &str) -> Result<PathBuf, String> {
    let internal = root.join(INTERNAL_VAULT_DIR);
    fs::create_dir_all(&internal).map_err(|e| format!("mkdir failed: {e}"))?;
    loop {
        let candidate = internal.join(format!("{prefix}{}", &uuid::Uuid::new_v4().simple().to_string()[..6]));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("mkdir failed: {error}")),
        }
    }
}

/// Erase a detached tree. Cleanup can be retried safely: nothing in it sits
/// at a live note path any more.
pub fn discard_temp_dir(dir: &Path, what: &str) {
    if let Err(error) = fs::remove_dir_all(dir) {
        if !not_found(&error) {
            eprintln!("{what} cleanup pending: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::layout;

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        layout::ensure_vault_layout(dir.path()).unwrap();
        dir
    }

    #[test]
    fn creation_date_is_recorded_once_and_read_back() {
        let v = vault();
        fs::write(v.path().join("inbox/A.md"), "a").unwrap();
        prepare_note_creation(v.path(), "inbox/A.md").unwrap();
        let path = note_metadata_path(v.path(), "inbox/A.md", false).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with("{\"version\":1,\"createdAt\":") && raw.ends_with("}\n"));
        let recorded = read_note_created_at(v.path(), "inbox/A.md", -1);
        assert!(recorded > 0);
        // A second save keeps the first date.
        fs::write(&path, "{\"version\":1,\"createdAt\":42}\n").unwrap();
        prepare_note_creation(v.path(), "inbox/A.md").unwrap();
        assert_eq!(read_note_created_at(v.path(), "inbox/A.md", -1), 42);
        // Corrupt metadata reads as the fallback, but blocks a save.
        fs::write(&path, "{\"version\":2}").unwrap();
        assert_eq!(read_note_created_at(v.path(), "inbox/A.md", 7), 7);
        assert!(prepare_note_creation(v.path(), "inbox/A.md").is_err());
        // Internal files and non-notes are never tracked.
        prepare_note_creation(v.path(), ".zennotes/x.md").unwrap();
        prepare_note_creation(v.path(), "inbox/pic.png").unwrap();
    }

    #[test]
    fn relocate_note_carries_every_sidecar() {
        let v = vault();
        let root = v.path();
        fs::write(root.join("inbox/A.md"), "a").unwrap();
        crate::vault::comments::write_note_comments(
            root,
            "inbox/A.md",
            &serde_json::json!([{ "body": "hi" }]),
        )
        .unwrap();
        prepare_note_creation(root, "inbox/A.md").unwrap();
        write_agent_edit_text(root, "inbox/A.md", Some("{}")).unwrap();

        fs::create_dir_all(root.join("archive/Sub")).unwrap();
        relocate_note(root, "inbox/A.md", &root.join("archive/Sub/A.md"), || Ok(())).unwrap();
        assert!(root.join("archive/Sub/A.md").exists());
        assert!(comments_path(root, "archive/Sub/A.md").exists());
        assert!(note_metadata_path(root, "archive/Sub/A.md", false).unwrap().exists());
        assert_eq!(read_agent_edit_text(root, "archive/Sub/A.md").as_deref(), Some("{}"));
        assert!(!comments_path(root, "inbox/A.md").exists());
    }

    #[test]
    fn relocate_refuses_leftover_comments_and_discards_orphan_dates() {
        let v = vault();
        let root = v.path();
        fs::write(root.join("inbox/A.md"), "a").unwrap();
        crate::vault::comments::write_note_comments(
            root,
            "inbox/B.md",
            &serde_json::json!([{ "body": "orphan" }]),
        )
        .unwrap();
        let err = relocate_note(root, "inbox/A.md", &root.join("inbox/B.md"), || Ok(())).unwrap_err();
        assert!(err.contains("Comments from an earlier note named “B”"), "{err}");
        assert!(root.join("inbox/A.md").exists());

        // An orphan creation date is not a reason to refuse: it is dropped.
        let orphan = note_metadata_path(root, "inbox/C.md", false).unwrap();
        fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        fs::write(&orphan, "{\"version\":1,\"createdAt\":5}\n").unwrap();
        relocate_note(root, "inbox/A.md", &root.join("inbox/C.md"), || Ok(())).unwrap();
        assert!(!orphan.exists() || read_note_created_at(root, "inbox/C.md", 0) != 5);
    }

    #[test]
    fn a_failed_persist_puts_everything_back() {
        let v = vault();
        let root = v.path();
        fs::write(root.join("inbox/A.md"), "a").unwrap();
        crate::vault::comments::write_note_comments(
            root,
            "inbox/A.md",
            &serde_json::json!([{ "body": "hi" }]),
        )
        .unwrap();
        let err = relocate_note(root, "inbox/A.md", &root.join("inbox/Z.md"), || Err("nope".into()))
            .unwrap_err();
        assert_eq!(err, "nope");
        assert!(root.join("inbox/A.md").exists());
        assert!(comments_path(root, "inbox/A.md").exists());
        assert!(!root.join("inbox/Z.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_moved_symlink_keeps_pointing_at_its_file() {
        let v = vault();
        let root = v.path();
        fs::create_dir_all(root.join("sources")).unwrap();
        fs::write(root.join("sources/X.md"), "x").unwrap();
        std::os::unix::fs::symlink("../sources/X.md", root.join("inbox/X.md")).unwrap();
        fs::create_dir_all(root.join("inbox/Topics")).unwrap();
        relocate_note(root, "inbox/X.md", &root.join("inbox/Topics/X.md"), || Ok(())).unwrap();
        assert_eq!(
            fs::read_link(root.join("inbox/Topics/X.md")).unwrap(),
            PathBuf::from("../../sources/X.md")
        );
        assert_eq!(fs::read_to_string(root.join("inbox/Topics/X.md")).unwrap(), "x");
    }

    #[cfg(unix)]
    #[test]
    fn respelling_a_link_never_retargets_it() {
        let v = vault();
        let root = v.path();
        fs::create_dir_all(root.join("inbox/Topics")).unwrap();
        fs::write(root.join("inbox/X.md"), "x").unwrap();
        let link = root.join("inbox/Topics/X.md");
        std::os::unix::fs::symlink("../X.md", &link).unwrap();
        // Another spelling of the same file is fine.
        respell_link(&link, "./../X.md").unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("./../X.md"));
        // Anything else is refused and the link is left as it was.
        assert!(respell_link(&link, "/etc/passwd").is_err());
        assert!(respell_link(&link, "../../../outside.md").is_err());
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("./../X.md"));
        // And a plain file is not a link to respell.
        assert!(respell_link(&root.join("inbox/X.md"), "X.md").is_err());
    }

    #[test]
    fn metadata_paths_refuse_escapes() {
        let v = vault();
        assert!(note_metadata_path(v.path(), "../../etc/passwd", false).is_err());
        assert!(agent_edit_path(v.path(), "../../x").is_err());
    }

    #[test]
    fn agent_edit_listing_finds_records_by_note() {
        let v = vault();
        write_agent_edit_text(v.path(), "inbox/Deep/A.md", Some("{\"version\":1}")).unwrap();
        let listed = list_agent_edit_texts(v.path());
        assert_eq!(listed, vec![("inbox/Deep/A.md".to_string(), "{\"version\":1}".to_string())]);
        write_agent_edit_text(v.path(), "inbox/Deep/A.md", None).unwrap();
        assert!(list_agent_edit_texts(v.path()).is_empty());
        assert!(!agent_edits_root(v.path()).exists());
    }
}
