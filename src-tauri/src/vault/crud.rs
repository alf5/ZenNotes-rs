//! Note create / write / rename / move / archive / trash / delete /
//! duplicate / append — port of the mutating helpers in
//! apps/desktop/src/main/vault.ts.

use std::fs;
use std::path::Path;

use crate::ipc::types::NoteMeta;
use crate::vault::atomic::write_file_atomic;
use crate::vault::comments;
use crate::vault::layout;
use crate::vault::notes::{self, folder_for_relative_path, read_meta, resolve_safe, to_posix};
use crate::vault::sidecars;

/// Port of `sanitizeNoteTitle`: strip path-hostile chars, collapse spaces,
/// cap at 200 chars, default to "Untitled".
pub fn sanitize_note_title(raw: Option<&str>) -> String {
    let raw = raw.unwrap_or("");
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if (c as u32) <= 0x1f => '-',
            c => c,
        })
        .collect();
    // Collapse runs of whitespace to a single space.
    let mut collapsed = String::with_capacity(replaced.len());
    let mut prev_space = false;
    for c in replaced.chars() {
        if c.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(c);
            prev_space = false;
        }
    }
    let trimmed: String = collapsed.trim().chars().take(200).collect();
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed
    }
}

/// `uniqueTitle` — first free `<base>.<ext>`, else `<base> 2.<ext>`, …
fn unique_title(dir: &Path, base_title: &str, ext: &str) -> String {
    let mut candidate = base_title.to_string();
    let mut n = 1;
    loop {
        if !dir.join(format!("{candidate}.{ext}")).exists() {
            return candidate;
        }
        n += 1;
        candidate = format!("{base_title} {n}");
    }
}

/// `uniqueFilename` — the first free name for `filename` in `dir`, keeping
/// its extension: `Note.md`, `Note 2.md`, `Note 3.md`, …
fn unique_filename(dir: &Path, filename: &str) -> String {
    let (base, ext) = match filename.rfind('.') {
        Some(idx) if idx > 0 => (&filename[..idx], &filename[idx..]),
        _ => (filename, ""),
    };
    let mut candidate = filename.to_string();
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = format!("{base} {n}{ext}");
        n += 1;
    }
    candidate
}

/// A brand-new file at `rel`: any creation date or agent edit record left
/// at that name belongs to a note that is gone, never to this one.
fn forget_stale_sidecars(root: &Path, abs: &Path) -> Result<(), String> {
    let rel = sidecars::rel_of(root, abs);
    sidecars::remove_note_creation(root, &rel, false)?;
    sidecars::remove_agent_edit_record(root, &rel, false)
}

fn rel_of(root: &Path, abs: &Path) -> String {
    let root_abs = crate::vault::config::resolve_path(&root.to_string_lossy());
    let abs_str = crate::vault::config::resolve_path(&abs.to_string_lossy());
    let rel = abs_str
        .strip_prefix(&format!("{root_abs}{}", std::path::MAIN_SEPARATOR))
        .unwrap_or(&abs_str);
    to_posix(rel)
}

fn folder_of(root: &Path, abs: &Path) -> Option<String> {
    folder_for_relative_path(&rel_of(root, abs))
}

/// `writeNote`. Atomic on purpose (#585): see vault/atomic.rs. Because the
/// save replaces the inode, the note's creation date is recorded first, except
/// in a temporary folder session, which never gets `.zennotes` state.
pub fn write_note(root: &Path, rel: &str, body: &str, ephemeral: bool) -> Result<NoteMeta, String> {
    let abs = resolve_safe(root, rel)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    if !ephemeral {
        sidecars::prepare_note_creation(root, &sidecars::rel_of(root, &abs))?;
    }
    write_file_atomic(&abs, body)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    read_meta(root, &abs, &folder, None, None)
}

pub fn append_to_note(root: &Path, rel: &str, body: &str, position: &str) -> Result<NoteMeta, String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let existing = fs::read_to_string(&abs).map_err(|e| format!("read failed: {e}"))?;
    let trimmed_addition = body.trim_end();
    if trimmed_addition.is_empty() {
        return read_meta(root, &abs, &folder, None, None);
    }
    let next = if position == "end" {
        let sep = if existing.ends_with('\n') { "" } else { "\n" };
        format!("{existing}{sep}\n{trimmed_addition}\n")
    } else {
        format!("{trimmed_addition}\n\n{existing}")
    };
    fs::write(&abs, next).map_err(|e| format!("write failed: {e}"))?;
    read_meta(root, &abs, &folder, None, None)
}

pub fn create_note(
    root: &Path,
    folder: &str,
    title: Option<&str>,
    subpath: &str,
) -> Result<NoteMeta, String> {
    let base = sanitize_note_title(title);
    let clean = subpath.trim_matches('/');
    let top_root = layout::folder_root(root, folder);
    let dir = if clean.is_empty() {
        top_root
    } else {
        resolve_safe(&top_root, clean)?
    };
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let final_title = unique_title(&dir, &base, "md");
    let abs = dir.join(format!("{final_title}.md"));
    forget_stale_sidecars(root, &abs)?;
    fs::write(&abs, format!("# {final_title}\n\n")).map_err(|e| format!("write failed: {e}"))?;
    read_meta(root, &abs, folder, None, None)
}

/// `vault:create-excalidraw` — `createNote` with an `.excalidraw` extension
/// and an empty native Excalidraw scene (upstream vault.ts:3152).
pub fn create_excalidraw(
    root: &Path,
    folder: &str,
    title: Option<&str>,
    subpath: &str,
) -> Result<NoteMeta, String> {
    let base = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(_) => sanitize_note_title(title),
        None => "Untitled drawing".to_string(),
    };
    let clean = subpath.trim_matches('/');
    let top_root = layout::folder_root(root, folder);
    let dir = if clean.is_empty() {
        top_root
    } else {
        resolve_safe(&top_root, clean)?
    };
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let final_title = unique_title(&dir, &base, "excalidraw");
    let abs = dir.join(format!("{final_title}.excalidraw"));
    let seed = serde_json::json!({
        "type": "excalidraw",
        "version": 2,
        "source": "zennotes",
        "elements": [],
        "appState": {},
        "files": {}
    });
    let body = serde_json::to_string_pretty(&seed).map_err(|e| e.to_string())?;
    forget_stale_sidecars(root, &abs)?;
    fs::write(&abs, body).map_err(|e| format!("write failed: {e}"))?;
    read_meta(root, &abs, folder, None, None)
}

/// Create a database record page (`<Name>.base/<title>.md`) with the caller's
/// pre-composed body; returns its vault-relative path (upstream
/// databases.ts:305). The sidecar's `pages` map is updated by the frontend.
pub fn create_database_record_page(
    root: &Path,
    form_dir_rel: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let dir_abs = resolve_safe(root, form_dir_rel)?;
    fs::create_dir_all(&dir_abs).map_err(|e| format!("mkdir failed: {e}"))?;
    let final_title = unique_title(&dir_abs, &sanitize_note_title(Some(title)), "md");
    let abs = dir_abs.join(format!("{final_title}.md"));
    fs::write(&abs, body).map_err(|e| format!("write failed: {e}"))?;
    Ok(format!("{}/{final_title}.md", form_dir_rel.trim_matches('/')))
}

/// Write a native `.excalidraw` drawing next to its source (used by the
/// Obsidian conversion, upstream vault.ts:3184): dedupe `<base>.excalidraw`
/// in `dir_rel`, write the scene body, return the new file's meta.
pub fn write_drawing_file(
    root: &Path,
    dir_rel: &str,
    base_title: &str,
    body: &str,
) -> Result<NoteMeta, String> {
    let clean_dir = dir_rel.trim_matches('/');
    let dir_abs = if clean_dir.is_empty() {
        root.to_path_buf()
    } else {
        resolve_safe(root, clean_dir)?
    };
    fs::create_dir_all(&dir_abs).map_err(|e| format!("mkdir failed: {e}"))?;
    let base = base_title.trim();
    let base = if base.is_empty() { "Untitled drawing" } else { base };
    let final_title = unique_title(&dir_abs, base, "excalidraw");
    let abs = dir_abs.join(format!("{final_title}.excalidraw"));
    forget_stale_sidecars(root, &abs)?;
    fs::write(&abs, body).map_err(|e| format!("write failed: {e}"))?;
    let rel = if clean_dir.is_empty() {
        format!("{final_title}.excalidraw")
    } else {
        format!("{clean_dir}/{final_title}.excalidraw")
    };
    let folder = notes::folder_for_relative_path(&rel)
        .ok_or_else(|| format!("Drawing is not in a known folder: {rel}"))?;
    read_meta(root, &abs, &folder, None, None)
}

pub fn rename_note(root: &Path, rel: &str, next_title: &str) -> Result<NoteMeta, String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
    let trimmed = sanitize_note_title(Some(next_title));
    // Preserve the note-like extension: renaming a drawing keeps .excalidraw.
    let ext = notes::note_extension(rel);
    let target = dir.join(format!("{trimmed}.{ext}"));

    if target != abs {
        // Said plainly, since the app shows this reason to whoever is
        // renaming. A case-only rename finds the note itself on a
        // case-insensitive disk.
        let source = sidecars::file_identity(&abs).map_err(|e| format!("stat failed: {e}"))?;
        match sidecars::file_identity(&target) {
            Ok(taken) if taken != source => {
                return Err(format!("A note named “{trimmed}” already exists in this folder"))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("stat failed: {e}")),
        }
    }
    let mut meta = None;
    sidecars::relocate_note(root, rel, &target, || {
        meta = Some(read_meta(root, &target, &folder, None, None)?);
        Ok(())
    })?;
    meta.ok_or_else(|| "rename failed".to_string())
}

fn folder_subpath_of(root: &Path, abs: &Path) -> String {
    let Some(folder) = folder_of(root, abs) else {
        return String::new();
    };
    let source_root = layout::folder_root(root, &folder);
    let Some(dir) = abs.parent() else {
        return String::new();
    };
    let source_root_abs = crate::vault::config::resolve_path(&source_root.to_string_lossy());
    let dir_abs = crate::vault::config::resolve_path(&dir.to_string_lossy());
    if dir_abs == source_root_abs {
        return String::new();
    }
    match dir_abs.strip_prefix(&format!("{source_root_abs}{}", std::path::MAIN_SEPARATOR)) {
        Some(rel) => to_posix(rel),
        None => String::new(),
    }
}

fn move_between_folders(root: &Path, rel: &str, target: &str) -> Result<NoteMeta, String> {
    let abs = resolve_safe(root, rel)?;
    let filename = abs
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let subpath = folder_subpath_of(root, &abs);
    let target_root = layout::folder_root(root, target);
    let dest_dir = if subpath.is_empty() {
        target_root
    } else {
        resolve_safe(&target_root, &subpath)?
    };
    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let dest_abs = dest_dir.join(unique_filename(&dest_dir, &filename));
    let mut meta = None;
    sidecars::relocate_note(root, rel, &dest_abs, || {
        meta = Some(read_meta(root, &dest_abs, target, None, None)?);
        Ok(())
    })?;
    meta.ok_or_else(|| "move failed".to_string())
}

pub fn move_to_trash(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    move_between_folders(root, rel, "trash")
}
pub fn restore_from_trash(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    move_between_folders(root, rel, "inbox")
}
pub fn archive_note(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    move_between_folders(root, rel, "archive")
}
pub fn unarchive_note(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    move_between_folders(root, rel, "inbox")
}

/// `emptyTrash`: the trash folder, its comment tree and its creation dates
/// are detached together into a temporary folder, then erased.
pub fn empty_trash(root: &Path) -> Result<(), String> {
    let trash_dir = layout::folder_root(root, "trash");
    let trash_rel = sidecars::rel_of(root, &trash_dir);
    let temporary = sidecars::internal_temp_dir(root, "trash-delete-")?;
    sidecars::relocate_folder_trees(
        vec![
            (trash_dir, temporary.join("content")),
            (comments::comments_root(root).join(&trash_rel), temporary.join("comments")),
            (sidecars::note_metadata_path(root, &trash_rel, true)?, temporary.join("metadata")),
        ],
        || Ok(()),
    )?;
    sidecars::discard_temp_dir(&temporary, "Trash");
    if let Err(error) = sidecars::remove_agent_edit_record(root, &trash_rel, true) {
        eprintln!("Agent edit cleanup pending: {error}");
    }
    Ok(())
}

/// `deleteNote`: the note leaves together with its comments and its
/// creation date, so a later note of the same name never inherits them.
pub fn delete_note(root: &Path, rel: &str, ephemeral: bool) -> Result<(), String> {
    let abs = resolve_safe(root, rel)?;
    let source = match fs::symlink_metadata(&abs) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("stat failed: {e}")),
    };
    if source.as_ref().is_some_and(|m| m.is_dir()) {
        return Err("Use the folder action to delete a directory.".into());
    }
    let note_rel = sidecars::rel_of(root, &abs);
    let comments = comments::comments_path(root, &note_rel);
    let has_comments = comments.exists();
    if ephemeral && !has_comments {
        let _ = fs::remove_file(&abs);
    } else {
        let temporary = sidecars::internal_temp_dir(root, "note-delete-")?;
        sidecars::relocate_folder_trees(
            vec![
                (abs.clone(), temporary.join("content")),
                (comments, temporary.join("comments")),
                (sidecars::note_metadata_path(root, &note_rel, false)?, temporary.join("metadata")),
            ],
            || Ok(()),
        )?;
        // Once detached, cleanup cannot attach the old discussion to a new note.
        sidecars::discard_temp_dir(&temporary, "Note");
    }
    if let Err(error) = sidecars::remove_agent_edit_record(root, &note_rel, false) {
        eprintln!("Agent edit cleanup pending: {error}");
    }
    Ok(())
}

pub fn move_note(
    root: &Path,
    old_rel: &str,
    target_folder: &str,
    target_subpath: &str,
) -> Result<NoteMeta, String> {
    let old_abs = resolve_safe(root, old_rel)?;
    let filename = old_abs
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let clean_sub = target_subpath.trim_matches('/');
    let target_root = layout::folder_root(root, target_folder);
    let dest_dir = if clean_sub.is_empty() {
        target_root
    } else {
        resolve_safe(&target_root, clean_sub)?
    };

    if old_abs.parent() == Some(dest_dir.as_path()) {
        let folder = folder_of(root, &old_abs)
            .ok_or_else(|| format!("Note not in a known folder: {old_rel}"))?;
        return read_meta(root, &old_abs, &folder, None, None);
    }

    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let dest_abs = dest_dir.join(unique_filename(&dest_dir, &filename));
    let mut meta = None;
    sidecars::relocate_note(root, old_rel, &dest_abs, || {
        meta = Some(read_meta(root, &dest_abs, target_folder, None, None)?);
        Ok(())
    })?;
    meta.ok_or_else(|| "move failed".to_string())
}

pub fn duplicate_note(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
    let base_title = notes::title_from_path(&abs);
    let ext = notes::note_extension(rel);
    let copy_title = unique_title(&dir, &format!("{base_title} copy"), ext);
    let dest_abs = dir.join(format!("{copy_title}.{ext}"));
    let body = fs::read_to_string(&abs).map_err(|e| format!("read failed: {e}"))?;
    forget_stale_sidecars(root, &dest_abs)?;
    fs::write(&dest_abs, body).map_err(|e| format!("write failed: {e}"))?;
    let meta = read_meta(root, &dest_abs, &folder, None, None)?;
    comments::copy_note_comments(root, rel, &meta.path);
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        layout::ensure_vault_layout(dir.path()).unwrap();
        dir
    }

    #[test]
    fn excalidraw_create_rename_move_preserve_extension() {
        let v = vault();
        let meta = create_excalidraw(v.path(), "inbox", None, "").unwrap();
        assert_eq!(meta.path, "inbox/Untitled drawing.excalidraw");
        assert!(meta.tags.is_empty() && meta.excerpt.is_empty());
        let body = fs::read_to_string(v.path().join("inbox/Untitled drawing.excalidraw")).unwrap();
        assert!(body.contains("\"type\": \"excalidraw\""));

        // De-dup probes the .excalidraw namespace, not .md.
        let second = create_excalidraw(v.path(), "inbox", None, "").unwrap();
        assert_eq!(second.path, "inbox/Untitled drawing 2.excalidraw");

        let renamed = rename_note(v.path(), &meta.path, "Sketch").unwrap();
        assert_eq!(renamed.path, "inbox/Sketch.excalidraw");

        let archived = archive_note(v.path(), &renamed.path).unwrap();
        assert_eq!(archived.path, "archive/Sketch.excalidraw");
    }

    #[test]
    fn sanitize_replaces_path_chars_and_caps() {
        assert_eq!(sanitize_note_title(Some("a/b:c*?")), "a-b-c--");
        assert_eq!(sanitize_note_title(Some("  many   spaces  ")), "many spaces");
        assert_eq!(sanitize_note_title(Some("")), "Untitled");
        assert_eq!(sanitize_note_title(None), "Untitled");
    }

    #[test]
    fn create_dedupes_titles() {
        let v = vault();
        let a = create_note(v.path(), "inbox", Some("Note"), "").unwrap();
        let b = create_note(v.path(), "inbox", Some("Note"), "").unwrap();
        assert_eq!(a.path, "inbox/Note.md");
        assert_eq!(b.path, "inbox/Note 2.md");
    }

    #[test]
    fn write_then_read_roundtrips_metadata() {
        let v = vault();
        create_note(v.path(), "inbox", Some("X"), "").unwrap();
        let meta = write_note(v.path(), "inbox/X.md", "# X\n\n#tag body", false).unwrap();
        assert_eq!(meta.tags, vec!["tag".to_string()]);
    }

    #[test]
    fn archive_then_unarchive_preserves_subpath() {
        let v = vault();
        create_note(v.path(), "inbox", Some("Deep"), "proj").unwrap();
        let archived = archive_note(v.path(), "inbox/proj/Deep.md").unwrap();
        assert_eq!(archived.path, "archive/proj/Deep.md");
        let restored = unarchive_note(v.path(), "archive/proj/Deep.md").unwrap();
        assert_eq!(restored.path, "inbox/proj/Deep.md");
    }

    #[test]
    fn trash_restore_and_empty() {
        let v = vault();
        create_note(v.path(), "inbox", Some("Temp"), "").unwrap();
        let trashed = move_to_trash(v.path(), "inbox/Temp.md").unwrap();
        assert_eq!(trashed.path, "trash/Temp.md");
        let restored = restore_from_trash(v.path(), "trash/Temp.md").unwrap();
        assert_eq!(restored.path, "inbox/Temp.md");
        move_to_trash(v.path(), "inbox/Temp.md").unwrap();
        empty_trash(v.path()).unwrap();
        assert!(!v.path().join("trash/Temp.md").exists());
    }

    #[test]
    fn duplicate_appends_copy_suffix() {
        let v = vault();
        create_note(v.path(), "inbox", Some("Orig"), "").unwrap();
        let dup = duplicate_note(v.path(), "inbox/Orig.md").unwrap();
        assert_eq!(dup.path, "inbox/Orig copy.md");
    }

    #[test]
    fn rename_moves_file() {
        let v = vault();
        create_note(v.path(), "inbox", Some("Before"), "").unwrap();
        let renamed = rename_note(v.path(), "inbox/Before.md", "After").unwrap();
        assert_eq!(renamed.path, "inbox/After.md");
        assert!(!v.path().join("inbox/Before.md").exists());
    }

    #[test]
    fn append_adds_at_end_and_start() {
        let v = vault();
        write_note(v.path(), "inbox/A.md", "body", false).unwrap();
        append_to_note(v.path(), "inbox/A.md", "tail", "end").unwrap();
        let after_end = fs::read_to_string(v.path().join("inbox/A.md")).unwrap();
        assert!(after_end.contains("body") && after_end.trim_end().ends_with("tail"));
        append_to_note(v.path(), "inbox/A.md", "head", "start").unwrap();
        let after_start = fs::read_to_string(v.path().join("inbox/A.md")).unwrap();
        assert!(after_start.starts_with("head\n\n"));
    }
}
