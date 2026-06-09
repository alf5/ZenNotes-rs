//! Vault directory walks — port of `listNotes` / `listFolders` from
//! apps/desktop/src/main/vault.ts, including the symlink-cycle guard
//! (`resolveDirDescent` + an `ancestors` set of realpaths).

use std::collections::HashSet;
use std::fs::{self, DirEntry, FileType};
use std::path::{Path, PathBuf};

use crate::ipc::types::{FolderEntry, NoteMeta};
use crate::vault::config::resolve_path;
use crate::vault::layout::{self, FOLDERS};
use crate::vault::notes;

fn realpath_or_resolve(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(resolve_path(&p.to_string_lossy())))
}

/// Mirror of `resolveDirDescent`: returns the realpath to descend into, or
/// `None` when the entry isn't a directory we should follow (or would form a
/// cycle). Plain dirs use the logical `parent_real/name`; symlinked dirs use
/// the canonical target.
fn resolve_dir_descent(
    full: &Path,
    file_type: &FileType,
    name: &str,
    parent_real: &Path,
    ancestors: &HashSet<PathBuf>,
) -> Option<PathBuf> {
    let real = if file_type.is_dir() {
        parent_real.join(name)
    } else if file_type.is_symlink() {
        match fs::metadata(full) {
            Ok(md) if md.is_dir() => fs::canonicalize(full).ok()?,
            _ => return None,
        }
    } else {
        return None;
    };
    if ancestors.contains(&real) {
        None
    } else {
        Some(real)
    }
}

fn is_markdown_note_entry(full: &Path, file_type: &FileType, name: &str) -> bool {
    if !name.to_lowercase().ends_with(".md") {
        return false;
    }
    if file_type.is_file() {
        return true;
    }
    if file_type.is_symlink() {
        return fs::metadata(full).map(|m| m.is_file()).unwrap_or(false);
    }
    false
}

/// Read a directory into an ordered Vec of (entry, file_type, name). Mirrors
/// the single readdir whose index Electron uses for `siblingOrder`.
fn read_dir_ordered(dir: &Path) -> Vec<(DirEntry, FileType, String)> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        out.push((entry, ft, name));
    }
    out
}

struct NoteFile {
    full: PathBuf,
    folder: String,
    sibling_order: i64,
    is_symlink: bool,
}

#[allow(clippy::too_many_arguments)]
fn walk_notes(
    folder: &str,
    dir_abs: &Path,
    dir_real: &Path,
    top_abs: &Path,
    is_primary_root: bool,
    ancestors: &mut HashSet<PathBuf>,
    out: &mut Vec<NoteFile>,
) {
    for (index, (entry, ft, name)) in read_dir_ordered(dir_abs).into_iter().enumerate() {
        let full = entry.path();
        if let Some(child_real) = resolve_dir_descent(&full, &ft, &name, dir_real, ancestors) {
            if name.starts_with('.') {
                continue;
            }
            if is_primary_root && dir_abs == top_abs && layout::should_hide_primary_root_entry(&name)
            {
                continue;
            }
            ancestors.insert(child_real.clone());
            walk_notes(folder, &full, &child_real, top_abs, is_primary_root, ancestors, out);
            ancestors.remove(&child_real);
            continue;
        }
        if is_markdown_note_entry(&full, &ft, &name) {
            out.push(NoteFile {
                full,
                folder: folder.to_string(),
                sibling_order: index as i64,
                is_symlink: ft.is_symlink(),
            });
        }
    }
}

/// `vault:list-notes` — all notes across inbox/quick/archive/trash.
pub fn list_notes(root: &Path) -> Vec<NoteMeta> {
    let mut files: Vec<NoteFile> = Vec::new();
    for folder in FOLDERS {
        let top_abs = layout::folder_root(root, folder);
        let is_primary_root = folder == "inbox" && resolve_path(&top_abs.to_string_lossy()) == resolve_path(&root.to_string_lossy());
        let top_real = realpath_or_resolve(&top_abs);
        let mut ancestors: HashSet<PathBuf> = HashSet::from([top_real.clone()]);
        walk_notes(folder, &top_abs, &top_real, &top_abs, is_primary_root, &mut ancestors, &mut files);
    }
    files
        .into_iter()
        .filter_map(|f| {
            notes::read_meta(root, &f.full, &f.folder, Some(f.sibling_order), Some(f.is_symlink)).ok()
        })
        .collect()
}

fn walk_folders(
    folder: &str,
    dir_abs: &Path,
    dir_real: &Path,
    top_abs: &Path,
    is_primary_root: bool,
    subpath: &str,
    ancestors: &mut HashSet<PathBuf>,
    out: &mut Vec<FolderEntry>,
) {
    for (index, (entry, ft, name)) in read_dir_ordered(dir_abs).into_iter().enumerate() {
        let full = entry.path();
        let Some(child_real) = resolve_dir_descent(&full, &ft, &name, dir_real, ancestors) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if is_primary_root && dir_abs == top_abs && layout::should_hide_primary_root_entry(&name) {
            continue;
        }
        let next_sub = if subpath.is_empty() {
            name.clone()
        } else {
            format!("{subpath}/{name}")
        };
        out.push(FolderEntry {
            folder: folder.to_string(),
            subpath: next_sub.clone(),
            sibling_order: index as i64,
            is_symlink: ft.is_symlink(),
        });
        ancestors.insert(child_real.clone());
        walk_folders(folder, &full, &child_real, top_abs, is_primary_root, &next_sub, ancestors, out);
        ancestors.remove(&child_real);
    }
}

/// `vault:list-folders` — every subfolder under the top-level folders.
pub fn list_folders(root: &Path) -> Vec<FolderEntry> {
    let mut out: Vec<FolderEntry> = Vec::new();
    for folder in FOLDERS {
        let top_abs = layout::folder_root(root, folder);
        let is_primary_root = folder == "inbox" && resolve_path(&top_abs.to_string_lossy()) == resolve_path(&root.to_string_lossy());
        let top_real = realpath_or_resolve(&top_abs);
        let mut ancestors: HashSet<PathBuf> = HashSet::from([top_real.clone()]);
        walk_folders(folder, &top_abs, &top_real, &top_abs, is_primary_root, "", &mut ancestors, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_notes_across_folders_and_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        layout::ensure_vault_layout(root).unwrap(); // seeds inbox/Welcome.md
        fs::write(root.join("inbox/A.md"), "# A #tag").unwrap();
        fs::create_dir_all(root.join("inbox/sub")).unwrap();
        fs::write(root.join("inbox/sub/B.md"), "see [[A]]").unwrap();
        fs::write(root.join("archive/C.md"), "archived").unwrap();
        fs::write(root.join("inbox/notes.txt"), "ignored").unwrap();

        let metas = list_notes(root);
        let paths: HashSet<String> = metas.iter().map(|m| m.path.clone()).collect();
        assert!(paths.contains("inbox/A.md"));
        assert!(paths.contains("inbox/sub/B.md"));
        assert!(paths.contains("archive/C.md"));
        assert!(paths.contains("inbox/Welcome.md"));
        assert!(!paths.iter().any(|p| p.ends_with(".txt")));

        let a = metas.iter().find(|m| m.path == "inbox/A.md").unwrap();
        assert_eq!(a.title, "A");
        assert_eq!(a.folder, "inbox");
        assert_eq!(a.tags, vec!["tag".to_string()]);
    }

    #[test]
    fn lists_subfolders_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        layout::ensure_vault_layout(root).unwrap();
        fs::create_dir_all(root.join("inbox/Projects/Sub")).unwrap();
        fs::create_dir_all(root.join("archive/Old")).unwrap();

        let folders = list_folders(root);
        let subs: HashSet<(String, String)> =
            folders.iter().map(|f| (f.folder.clone(), f.subpath.clone())).collect();
        assert!(subs.contains(&("inbox".into(), "Projects".into())));
        assert!(subs.contains(&("inbox".into(), "Projects/Sub".into())));
        assert!(subs.contains(&("archive".into(), "Old".into())));
    }
}
