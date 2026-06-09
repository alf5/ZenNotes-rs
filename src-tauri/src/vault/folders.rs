//! Folder operations — port of `createFolder`, `renameFolder`, `deleteFolder`,
//! `duplicateFolder` from apps/desktop/src/main/vault.ts, including the
//! folder-icon rewrites in vault settings.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::vault::config::resolve_path;
use crate::vault::layout;
use crate::vault::notes::{resolve_safe, to_posix};
use crate::vault::settings;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
    if new_abs == old_abs {
        return Ok(new_clean);
    }
    let sep = std::path::MAIN_SEPARATOR;
    let old_str = old_abs.to_string_lossy().to_string();
    let new_str = new_abs.to_string_lossy().to_string();
    if format!("{new_str}{sep}").starts_with(&format!("{old_str}{sep}")) {
        return Err("Cannot move a folder into itself".into());
    }

    let same_dir = match (fs::canonicalize(&old_abs), fs::canonicalize(&new_abs)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if new_abs.exists() && !same_dir {
        return Err(format!("A folder already exists at \"{new_clean}\""));
    }
    if let Some(parent) = new_abs.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    if old_str.to_lowercase() == new_str.to_lowercase() && old_str != new_str {
        let tmp = PathBuf::from(format!("{old_str}_rename_tmp_{}", now_ms()));
        fs::rename(&old_abs, &tmp).map_err(|e| format!("rename failed: {e}"))?;
        fs::rename(&tmp, &new_abs).map_err(|e| format!("rename failed: {e}"))?;
    } else {
        fs::rename(&old_abs, &new_abs).map_err(|e| format!("rename failed: {e}"))?;
    }

    let folder = top_folder.to_string();
    let (oc, nc) = (old_clean.clone(), new_clean.clone());
    settings::update_folder_icons(root, |icons| {
        settings::rewrite_folder_icons_for_rename(icons, &folder, &oc, &nc)
    })?;
    Ok(new_clean)
}

pub fn delete_folder(root: &Path, top_folder: &str, subpath: &str) -> Result<(), String> {
    let clean = trim_slashes(subpath);
    if clean.is_empty() {
        return Err("Cannot delete the top-level folder".into());
    }
    let abs = resolve_safe(&layout::folder_root(root, top_folder), &clean)?;
    let _ = fs::remove_dir_all(&abs);
    let folder = top_folder.to_string();
    settings::update_folder_icons(root, |icons| {
        settings::remove_folder_icons(icons, &folder, &clean)
    })?;
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
        delete_folder(v.path(), "inbox", "Work").unwrap();
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
