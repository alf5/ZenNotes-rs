//! Note reading + metadata assembly — port of `readMeta`, `readNote`,
//! `resolveSafe`, `folderForRelativePath` from apps/desktop/src/main/vault.ts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::{NoteContent, NoteMeta};
use crate::vault::config::resolve_path;
use crate::vault::layout;
use crate::vault::metadata;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Convert native path separators to POSIX.
pub fn to_posix(p: &str) -> String {
    p.replace('\\', "/")
}

/// Lexically normalize a vault-relative path to POSIX, stripping `./` prefixes.
pub fn normalize_vault_relative_path(rel: &str) -> String {
    let posix = to_posix(rel);
    let mut out: Vec<&str> = Vec::new();
    let is_abs = posix.starts_with('/');
    for seg in posix.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                } else if !is_abs {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if joined == "." {
        String::new()
    } else {
        joined
    }
}

/// Classify a vault-relative path into a top-level folder. Mirrors
/// `folderForRelativePath`: system folders map to themselves, dot/reserved
/// roots map to nothing, everything else is a primary ("inbox") note.
pub fn folder_for_relative_path(rel: &str) -> Option<String> {
    let normalized = normalize_vault_relative_path(rel);
    let top = normalized.split('/').next().unwrap_or("");
    if layout::is_system_folder(top) {
        return Some(top.to_string());
    }
    if top.is_empty() || top.starts_with('.') {
        return None;
    }
    if layout::is_reserved_root_name(top) {
        return None;
    }
    Some("inbox".to_string())
}

/// Resolve a vault-relative path to an absolute path, rejecting any path that
/// escapes the vault root. Mirrors `resolveSafe`.
pub fn resolve_safe(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let root_abs = resolve_path(&root.to_string_lossy());
    let candidate = if Path::new(rel).is_absolute() {
        rel.to_string()
    } else {
        format!("{}/{}", root_abs, to_posix(rel))
    };
    let abs = resolve_path(&candidate);
    let sep = std::path::MAIN_SEPARATOR;
    if abs != root_abs && !abs.starts_with(&format!("{root_abs}{sep}")) {
        return Err(format!("Path escapes vault: {rel}"));
    }
    Ok(PathBuf::from(abs))
}

fn system_time_to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `createdAt` mirrors Electron's `stat.birthtimeMs || stat.ctimeMs`.
fn created_at_ms(md: &fs::Metadata, modified_ms: i64) -> i64 {
    if let Ok(created) = md.created() {
        return system_time_to_ms(created);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = modified_ms;
        return md.ctime() * 1000 + (md.ctime_nsec() / 1_000_000);
    }
    #[cfg(not(unix))]
    {
        modified_ms
    }
}

/// File name without its last extension. Mirrors `path.basename(abs, extname)`.
pub fn title_from_path(abs: &Path) -> String {
    let name = abs
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    match name.rfind('.') {
        // A leading dot (e.g. ".bashrc") is not an extension.
        Some(idx) if idx > 0 => name[..idx].to_string(),
        _ => name,
    }
}

fn read_sibling_order(abs: &Path) -> i64 {
    let Some(parent) = abs.parent() else {
        return MAX_SAFE_INTEGER;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return MAX_SAFE_INTEGER;
    };
    let name = abs.file_name();
    for (index, entry) in entries.enumerate() {
        if let Ok(entry) = entry {
            if Some(entry.file_name().as_os_str()) == name {
                return index as i64;
            }
        }
    }
    MAX_SAFE_INTEGER
}

/// Build a `NoteMeta` from a note file. `sibling_order`/`is_symlink` are passed
/// in by the directory walk; `read_note` (single file) passes `None`.
pub fn read_meta(
    root: &Path,
    abs: &Path,
    folder: &str,
    sibling_order: Option<i64>,
    is_symlink: Option<bool>,
) -> Result<NoteMeta, String> {
    let md = fs::metadata(abs).map_err(|e| format!("stat failed: {e}"))?;
    let root_abs = resolve_path(&root.to_string_lossy());
    let abs_str = resolve_path(&abs.to_string_lossy());
    let rel = abs_str
        .strip_prefix(&format!("{root_abs}{}", std::path::MAIN_SEPARATOR))
        .unwrap_or(&abs_str);
    let rel_path = to_posix(rel);

    let linked = is_symlink.unwrap_or_else(|| {
        fs::symlink_metadata(abs)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    });
    let resolved_sibling = sibling_order.unwrap_or_else(|| read_sibling_order(abs));

    let modified_ms = md.modified().map(system_time_to_ms).unwrap_or(0);
    let body = fs::read_to_string(abs).unwrap_or_default();

    Ok(NoteMeta {
        path: rel_path,
        title: title_from_path(abs),
        folder: folder.to_string(),
        sibling_order: resolved_sibling,
        created_at: created_at_ms(&md, modified_ms),
        updated_at: modified_ms,
        size: md.len(),
        tags: metadata::extract_tags(&body),
        wikilinks: metadata::extract_wikilinks(&body),
        has_attachments: metadata::body_has_local_asset(&body),
        excerpt: metadata::build_excerpt(&body),
        is_symlink: linked,
    })
}

/// `vault:read-note` — full note content (metadata + raw body).
pub fn read_note(root: &Path, rel: &str) -> Result<NoteContent, String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_for_relative_path(rel)
        .ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let body = fs::read_to_string(&abs).map_err(|e| format!("read failed: {e}"))?;
    let meta = read_meta(root, &abs, &folder, None, None)?;
    Ok(NoteContent { meta, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_classification() {
        assert_eq!(folder_for_relative_path("inbox/A.md").as_deref(), Some("inbox"));
        assert_eq!(folder_for_relative_path("archive/B.md").as_deref(), Some("archive"));
        // Root-mode primary note (not a system/reserved folder) → inbox.
        assert_eq!(folder_for_relative_path("Note.md").as_deref(), Some("inbox"));
        assert_eq!(folder_for_relative_path(".zennotes/x").as_deref(), None);
        assert_eq!(folder_for_relative_path("attachements/x.png").as_deref(), None);
    }

    #[test]
    fn resolve_safe_rejects_escapes() {
        let root = Path::new("/tmp/vault");
        assert!(resolve_safe(root, "inbox/a.md").is_ok());
        assert!(resolve_safe(root, "../secret.md").is_err());
        assert!(resolve_safe(root, "inbox/../../etc/passwd").is_err());
    }

    #[test]
    fn title_strips_extension() {
        assert_eq!(title_from_path(Path::new("/v/Foo.md")), "Foo");
        assert_eq!(title_from_path(Path::new("/v/Foo.tar.md")), "Foo.tar");
    }
}
