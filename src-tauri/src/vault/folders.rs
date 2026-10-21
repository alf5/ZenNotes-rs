//! Folder operations — port of `createFolder`, `renameFolder`, `deleteFolder`,
//! `duplicateFolder` from apps/desktop/src/main/vault.ts, including the
//! folder-icon rewrites in vault settings.

use std::fs;
use std::path::Path;

use crate::vault::comments;
use crate::vault::config::resolve_path;
use crate::vault::layout;
use crate::vault::notes::{resolve_safe, to_posix};
use crate::vault::settings;
use crate::vault::sidecars;

fn trim_slashes(s: &str) -> String {
    s.trim_matches('/').to_string()
}

pub fn create_folder(root: &Path, top_folder: &str, subpath: &str) -> Result<(), String> {
    let trimmed = trim_slashes(subpath);
    if trimmed.is_empty() {
        return Err("Folder name is required".into());
    }
    let abs = resolve_safe(&layout::folder_root(root, top_folder), &trimmed)?;
    fs::create_dir_all(&abs).map_err(|e| format!("mkdir failed: {e}"))
}

/// Port of `renameFolderTrees`: the shared local folder move for ordinary
/// folders and database containers. The folder moves together with its
/// comment tree, its creation dates and its agent edit records; `persist`
/// (the settings write) runs inside the same transaction, and a failure
/// anywhere puts every tree back.
pub fn rename_folder_trees(
    root: &Path,
    old_rel: &str,
    new_rel: &str,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let old_abs = resolve_safe(root, old_rel)?;
    let new_abs = resolve_safe(root, new_rel)?;
    let root_abs = std::path::PathBuf::from(resolve_path(&root.to_string_lossy()));
    if old_abs == root_abs || new_abs == root_abs {
        return Err("Cannot rename the vault root".into());
    }
    fs::metadata(&old_abs).map_err(|e| format!("stat failed: {e}"))?;
    if old_abs == new_abs {
        return Ok(());
    }
    let sep = std::path::MAIN_SEPARATOR;
    if format!("{}{sep}", new_abs.display()).starts_with(&format!("{}{sep}", old_abs.display())) {
        return Err("Cannot move a folder into itself".into());
    }
    let old_rel = sidecars::rel_of(root, &old_abs);
    let new_rel = sidecars::rel_of(root, &new_abs);
    let comments_root = comments::comments_root(root);
    sidecars::relocate_folder_trees(
        vec![
            (old_abs, new_abs),
            (comments_root.join(&old_rel), comments_root.join(&new_rel)),
            (
                sidecars::note_metadata_path(root, &old_rel, true)?,
                sidecars::note_metadata_path(root, &new_rel, true)?,
            ),
            (
                sidecars::agent_edit_folder_path(root, &old_rel)?,
                sidecars::agent_edit_folder_path(root, &new_rel)?,
            ),
        ],
        persist,
    )
}

/// Rename or move a subfolder. `new_subpath` is the full target path relative
/// to the top folder, so `Work/Research` → `Projects/Research` also moves it
/// into `Projects`. Refuses to move into itself or a descendant, and refuses
/// to touch the top-level folder.
pub fn rename_folder(
    root: &Path,
    top_folder: &str,
    old_subpath: &str,
    new_subpath: &str,
) -> Result<String, String> {
    let old_clean = trim_slashes(old_subpath);
    let new_clean = trim_slashes(new_subpath);
    if old_clean.is_empty() {
        return Err("Cannot rename the top-level folder".into());
    }
    if new_clean.is_empty() {
        return Err("Target folder name is required".into());
    }
    let top_root = layout::folder_root(root, top_folder);
    let old_abs = resolve_safe(&top_root, &old_clean)?;
    let new_abs = resolve_safe(&top_root, &new_clean)?;
    fs::metadata(&old_abs).map_err(|e| format!("stat failed: {e}"))?;
    if new_abs == old_abs {
        return Ok(new_clean);
    }
    let sep = std::path::MAIN_SEPARATOR;
    if format!("{}{sep}", new_abs.display()).starts_with(&format!("{}{sep}", old_abs.display())) {
        return Err("Cannot move a folder into itself".into());
    }
    // Same file on both sides is a case-only rename on a case-insensitive disk.
    if let Ok(existing) = sidecars::file_identity(&new_abs) {
        let source = sidecars::file_identity(&old_abs).map_err(|e| format!("stat failed: {e}"))?;
        if existing != source {
            return Err(format!("A folder already exists at \"{new_clean}\""));
        }
    }
    let old_rel = sidecars::rel_of(root, &old_abs);
    let new_rel = sidecars::rel_of(root, &new_abs);
    let next = settings::after_folder_rename(
        settings::settings_value(root)?,
        top_folder,
        &old_clean,
        &new_clean,
        &old_rel,
        &new_rel,
    );
    rename_folder_trees(root, &old_rel, &new_rel, || {
        settings::set_vault_settings(root, &next).map(|_| ())
    })?;
    Ok(new_clean)
}

/// `deleteFolder`: the folder leaves together with its comment tree and its
/// creation dates (detached into a temporary folder in one transaction with
/// the settings write, then erased), and its agent edit records go too.
pub fn delete_folder(root: &Path, top_folder: &str, subpath: &str, ephemeral: bool) -> Result<(), String> {
    let clean = trim_slashes(subpath);
    if clean.is_empty() {
        return Err("Cannot delete the top-level folder".into());
    }
    let abs = resolve_safe(&layout::folder_root(root, top_folder), &clean)?;
    let rel = sidecars::rel_of(root, &abs);
    let comments = comments::comments_root(root).join(&rel);
    if ephemeral && !comments.exists() {
        let _ = fs::remove_dir_all(&abs);
        return sidecars::remove_agent_edit_record(root, &rel, true);
    }
    let next = settings::after_folder_delete(settings::settings_value(root)?, top_folder, &clean);
    let temporary = sidecars::internal_temp_dir(root, "folder-delete-")?;
    sidecars::relocate_folder_trees(
        vec![
            (abs, temporary.join("content")),
            (comments, temporary.join("comments")),
            (sidecars::note_metadata_path(root, &rel, true)?, temporary.join("metadata")),
        ],
        || settings::set_vault_settings(root, &next).map(|_| ()),
    )?;
    sidecars::discard_temp_dir(&temporary, "Folder");
    if let Err(error) = sidecars::remove_agent_edit_record(root, &rel, true) {
        eprintln!("Agent edit cleanup pending: {error}");
    }
    Ok(())
}

pub fn duplicate_folder(root: &Path, top_folder: &str, subpath: &str) -> Result<String, String> {
    let clean = trim_slashes(subpath);
    if clean.is_empty() {
        return Err("Cannot duplicate the top-level folder".into());
    }
    let top_root = layout::folder_root(root, top_folder);
    let old_abs = resolve_safe(&top_root, &clean)?;
    let parent = old_abs.parent().map(Path::to_path_buf).unwrap_or_default();
    let base_name = old_abs
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut copy_name = format!("{base_name} copy");
    let mut n = 1;
    while parent.join(&copy_name).exists() {
        n += 1;
        copy_name = format!("{base_name} copy {n}");
    }
    let new_abs = parent.join(&copy_name);
    copy_dir_recursive(&old_abs, &new_abs).map_err(|e| format!("copy failed: {e}"))?;

    let top_root_abs = resolve_path(&top_root.to_string_lossy());
    let new_abs_resolved = resolve_path(&new_abs.to_string_lossy());
    let new_subpath = to_posix(
        new_abs_resolved
            .strip_prefix(&format!("{top_root_abs}{}", std::path::MAIN_SEPARATOR))
            .unwrap_or(&new_abs_resolved),
    );

    let folder = top_folder.to_string();
    let (src, dst) = (clean.clone(), new_subpath.clone());
    settings::update_folder_icons(root, |icons| {
        settings::duplicate_folder_icons(icons, &folder, &src, &dst)
    })?;
    Ok(new_subpath)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
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
    fn create_rename_delete_folder() {
        let v = vault();
        create_folder(v.path(), "inbox", "Projects").unwrap();
        assert!(v.path().join("inbox/Projects").is_dir());
        let nc = rename_folder(v.path(), "inbox", "Projects", "Work").unwrap();
        assert_eq!(nc, "Work");
        assert!(v.path().join("inbox/Work").is_dir());
        assert!(!v.path().join("inbox/Projects").exists());
        delete_folder(v.path(), "inbox", "Work", false).unwrap();
        assert!(!v.path().join("inbox/Work").exists());
    }

    #[test]
    fn duplicate_folder_copies_contents() {
        let v = vault();
        create_folder(v.path(), "inbox", "Src/Deep").unwrap();
        fs::write(v.path().join("inbox/Src/a.md"), "a").unwrap();
        let new_sub = duplicate_folder(v.path(), "inbox", "Src").unwrap();
        assert_eq!(new_sub, "Src copy");
        assert!(v.path().join("inbox/Src copy/Deep").is_dir());
        assert!(v.path().join("inbox/Src copy/a.md").is_file());
    }

    #[test]
    fn rename_into_itself_is_rejected() {
        let v = vault();
        create_folder(v.path(), "inbox", "A").unwrap();
        assert!(rename_folder(v.path(), "inbox", "A", "A/B").is_err());
    }
}
