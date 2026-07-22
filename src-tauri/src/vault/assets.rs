//! Asset (attachment) operations — port of the asset helpers in
//! apps/desktop/src/main/vault.ts (`listAssets`, `hasAssetsDir`,
//! `importFiles`, `importPastedImage`, rename/move/duplicate/delete/restore).
//!
//! An asset is any non-`.md` file in the vault (outside `.zennotes`). New
//! imports land at the vault root, matching the Electron behaviour.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ipc::types::{AssetMeta, DeletedAsset, ImportedAsset};
use crate::vault::config::resolve_path;
use crate::vault::layout::{INTERNAL_VAULT_DIR, LEGACY_ATTACHMENTS_DIRS, PRIMARY_ATTACHMENTS_DIR};
use crate::vault::metadata::{classify_imported_asset, pasted_image_extension};
use crate::vault::notes::{normalize_vault_relative_path, resolve_safe, to_posix};

const DELETED_ASSETS_DIR: &str = "deleted-assets";
/// Per-token metadata sidecar inside each deleted-asset dir (upstream parity).
const DELETED_ASSET_META: &str = ".zn-deleted.json";

fn rel_of(root: &Path, abs: &Path) -> String {
    let root_abs = resolve_path(&root.to_string_lossy());
    let abs_str = resolve_path(&abs.to_string_lossy());
    let rel = abs_str
        .strip_prefix(&format!("{root_abs}{}", std::path::MAIN_SEPARATOR))
        .unwrap_or(&abs_str);
    to_posix(rel)
}

fn asset_meta_for_path(root: &Path, abs: &Path) -> Result<AssetMeta, String> {
    let md = fs::metadata(abs).map_err(|e| format!("stat failed: {e}"))?;
    let name = abs
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(AssetMeta {
        path: rel_of(root, abs),
        name: name.clone(),
        kind: classify_imported_asset(&name).to_string(),
        sibling_order: 0,
        size: md.len(),
        updated_at: md.modified().ok().and_then(sys_ms).unwrap_or(0),
    })
}

fn sys_ms(t: std::time::SystemTime) -> Option<i64> {
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

fn now_ms() -> i64 {
    sys_ms(std::time::SystemTime::now()).unwrap_or(0)
}

/// `uniqueFilename` — keep the name free of collisions: `foo.png`, `foo 2.png`…
fn unique_filename(dir: &Path, filename: &str) -> String {
    let (base, ext) = split_ext(filename);
    let mut candidate = filename.to_string();
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = format!("{base} {n}{ext}");
        n += 1;
    }
    candidate
}

/// Split into (stem, ".ext"); a leading dot is not an extension.
fn split_ext(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(idx) if idx > 0 => (name[..idx].to_string(), name[idx..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

fn assert_asset_file(root: &Path, rel: &str) -> Result<(String, PathBuf), String> {
    let normalized = normalize_vault_relative_path(rel);
    if normalized.is_empty() {
        return Err("Asset path is required.".into());
    }
    if normalized.split('/').any(|c| c == INTERNAL_VAULT_DIR) {
        return Err("Cannot modify internal ZenNotes-rs files.".into());
    }
    if normalized.to_lowercase().ends_with(".md") {
        return Err("Use note actions to modify markdown notes.".into());
    }
    let abs = resolve_safe(root, &normalized)?;
    let md = fs::metadata(&abs).map_err(|e| format!("stat failed: {e}"))?;
    if !md.is_file() {
        return Err("Asset path is not a file.".into());
    }
    Ok((normalized, abs))
}

fn clean_asset_filename(name: &str) -> Result<String, String> {
    let raw = name.trim();
    if raw.contains('/') || raw.contains('\\') {
        return Err("Use only a file name.".into());
    }
    let trimmed = Path::new(raw)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err("Asset name is required.".into());
    }
    if trimmed.to_lowercase().ends_with(".md") {
        return Err("Use note actions for markdown notes.".into());
    }
    Ok(trimmed)
}

pub fn has_assets_dir(root: &Path) -> bool {
    std::iter::once(PRIMARY_ATTACHMENTS_DIR)
        .chain(LEGACY_ATTACHMENTS_DIRS)
        .any(|d| root.join(d).is_dir())
}

/// `vault:list-assets` — every non-md file in the vault (excluding `.zennotes`
/// and dotfiles), most-recently-modified first.
pub fn list_assets(root: &Path) -> Vec<AssetMeta> {
    let mut out: Vec<AssetMeta> = Vec::new();
    let root_real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut ancestors: HashSet<PathBuf> = HashSet::from([root_real]);
    walk_assets(root, root, &mut ancestors, &mut out);
    out.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

fn walk_assets(root: &Path, dir: &Path, ancestors: &mut HashSet<PathBuf>, out: &mut Vec<AssetMeta>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let entries: Vec<_> = entries.flatten().collect();
    for (index, entry) in entries.iter().enumerate() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let full = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        let is_dir = ft.is_dir() || (ft.is_symlink() && full.is_dir());
        if is_dir {
            if dir == root && name == INTERNAL_VAULT_DIR {
                continue;
            }
            let Ok(real) = fs::canonicalize(&full) else { continue };
            if ancestors.contains(&real) {
                continue;
            }
            ancestors.insert(real.clone());
            walk_assets(root, &full, ancestors, out);
            ancestors.remove(&real);
            continue;
        }
        let lower = name.to_lowercase();
        // `.excalidraw` drawings are note-like (listed by list_notes), not
        // loose assets — mirror upstream vault.ts:3807.
        if !ft.is_file() || lower.ends_with(".md") || lower.ends_with(".excalidraw") {
            continue;
        }
        let Ok(md) = fs::metadata(&full) else { continue };
        out.push(AssetMeta {
            path: rel_of(root, &full),
            name: name.clone(),
            kind: classify_imported_asset(&name).to_string(),
            sibling_order: index as i64,
            size: md.len(),
            updated_at: md.modified().ok().and_then(sys_ms).unwrap_or(0),
        });
    }
}

pub fn rename_asset(root: &Path, rel: &str, next_name: &str) -> Result<AssetMeta, String> {
    let (_, abs) = assert_asset_file(root, rel)?;
    let clean = clean_asset_filename(next_name)?;
    let dest = abs.parent().map(|p| p.join(&clean)).unwrap_or_else(|| PathBuf::from(&clean));
    if dest != abs {
        let same = matches!(
            (fs::canonicalize(&abs), fs::canonicalize(&dest)),
            (Ok(a), Ok(b)) if a == b
        );
        if dest.exists() && !same {
            return Err(format!("An asset named \"{clean}\" already exists in this folder."));
        }
        if abs.to_string_lossy().to_lowercase() == dest.to_string_lossy().to_lowercase()
            && abs != dest
        {
            let tmp = PathBuf::from(format!("{}_rename_tmp_{}", abs.to_string_lossy(), now_ms()));
            fs::rename(&abs, &tmp).map_err(|e| format!("rename failed: {e}"))?;
            fs::rename(&tmp, &dest).map_err(|e| format!("rename failed: {e}"))?;
        } else {
            fs::rename(&abs, &dest).map_err(|e| format!("rename failed: {e}"))?;
        }
    }
    asset_meta_for_path(root, &dest)
}

pub fn move_asset(root: &Path, rel: &str, target_dir: &str) -> Result<AssetMeta, String> {
    let (_, abs) = assert_asset_file(root, rel)?;
    let normalized = normalize_vault_relative_path(target_dir);
    let normalized = normalized.trim_matches('/');
    if normalized.split('/').any(|c| c == INTERNAL_VAULT_DIR) {
        return Err("Cannot move assets into internal ZenNotes-rs files.".into());
    }
    let dest_dir = if normalized.is_empty() {
        root.to_path_buf()
    } else {
        resolve_safe(root, normalized)?
    };
    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    if resolve_path(&dest_dir.to_string_lossy())
        == resolve_path(&abs.parent().unwrap_or(root).to_string_lossy())
    {
        return asset_meta_for_path(root, &abs);
    }
    let name = abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let final_name = unique_filename(&dest_dir, &name);
    let dest = dest_dir.join(final_name);
    if dest != abs {
        fs::rename(&abs, &dest).map_err(|e| format!("move failed: {e}"))?;
    }
    asset_meta_for_path(root, &dest)
}

pub fn duplicate_asset(root: &Path, rel: &str) -> Result<AssetMeta, String> {
    let (_, abs) = assert_asset_file(root, rel)?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let (base, ext) = split_ext(&name);
    let final_name = unique_filename(&dir, &format!("{base} copy{ext}"));
    let dest = dir.join(final_name);
    fs::copy(&abs, &dest).map_err(|e| format!("copy failed: {e}"))?;
    asset_meta_for_path(root, &dest)
}

pub fn delete_asset(root: &Path, rel: &str) -> Result<DeletedAsset, String> {
    let (rel_norm, abs) = assert_asset_file(root, rel)?;
    let undo_token = uuid::Uuid::new_v4().to_string();
    let trash_dir = resolve_safe(
        root,
        &format!("{INTERNAL_VAULT_DIR}/{DELETED_ASSETS_DIR}/{undo_token}"),
    )?;
    fs::create_dir_all(&trash_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let name = abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    fs::rename(&abs, trash_dir.join(&name)).map_err(|e| format!("delete failed: {e}"))?;
    let deleted_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // `.zn-deleted.json` sidecar (upstream v2.11+): persists the original
    // location so the Trash view can list and restore assets across restarts.
    let sidecar = serde_json::json!({ "path": rel_norm, "name": name, "deletedAt": deleted_at });
    let _ = fs::write(
        trash_dir.join(DELETED_ASSET_META),
        serde_json::to_string_pretty(&sidecar).unwrap_or_else(|_| "{}".into()),
    );
    Ok(DeletedAsset { path: rel_norm, name, undo_token, deleted_at: Some(deleted_at) })
}

/// `vault:list-deleted-assets` — scan `.zennotes/deleted-assets/*` token dirs,
/// read each `.zn-deleted.json` sidecar, and skip entries whose metadata or
/// stored file is missing (pre-sidecar deletes). Newest first.
pub fn list_deleted_assets(root: &Path) -> Vec<DeletedAsset> {
    let dir = root.join(INTERNAL_VAULT_DIR).join(DELETED_ASSETS_DIR);
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let token = entry.file_name().to_string_lossy().to_string();
        if uuid::Uuid::parse_str(&token).is_err() || !entry.path().is_dir() {
            continue;
        }
        let token_dir = entry.path();
        let Ok(raw) = fs::read_to_string(token_dir.join(DELETED_ASSET_META)) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let (Some(path), Some(name)) = (
            meta.get("path").and_then(serde_json::Value::as_str),
            meta.get("name").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        if path.is_empty() || name.is_empty() || !token_dir.join(name).is_file() {
            continue;
        }
        out.push(DeletedAsset {
            path: path.to_string(),
            name: name.to_string(),
            undo_token: token,
            deleted_at: meta
                .get("deletedAt")
                .and_then(serde_json::Value::as_str)
                .map(String::from),
        });
    }
    out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    out
}

/// `vault:purge-deleted-asset` — permanently drop one token dir.
pub fn purge_deleted_asset(root: &Path, undo_token: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(undo_token).is_err() {
        return Err("Deleted asset token is invalid.".into());
    }
    let dir = root
        .join(INTERNAL_VAULT_DIR)
        .join(DELETED_ASSETS_DIR)
        .join(undo_token);
    let _ = fs::remove_dir_all(dir);
    Ok(())
}

/// `vault:empty-deleted-assets` — drop the whole store.
pub fn empty_deleted_assets(root: &Path) {
    let _ = fs::remove_dir_all(root.join(INTERNAL_VAULT_DIR).join(DELETED_ASSETS_DIR));
}

pub fn restore_deleted_asset(root: &Path, deleted: &DeletedAsset) -> Result<AssetMeta, String> {
    let target_rel = normalize_vault_relative_path(&deleted.path);
    if target_rel.is_empty() || target_rel.split('/').any(|c| c == INTERNAL_VAULT_DIR) {
        return Err("Cannot restore internal ZenNotes-rs files.".into());
    }
    if target_rel.to_lowercase().ends_with(".md") {
        return Err("Use note actions to restore markdown notes.".into());
    }
    let name = clean_asset_filename(&deleted.name)?;
    // Validate the undo token shape (uuid).
    if uuid::Uuid::parse_str(&deleted.undo_token).is_err() {
        return Err("Deleted asset restore token is invalid.".into());
    }
    let trash_dir = resolve_safe(
        root,
        &format!("{INTERNAL_VAULT_DIR}/{DELETED_ASSETS_DIR}/{}", deleted.undo_token),
    )?;
    let source = trash_dir.join(&name);
    let target_abs = resolve_safe(root, &target_rel)?;
    let target_dir = target_abs.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&target_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let target_name = target_abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let final_name = unique_filename(&target_dir, &target_name);
    let final_abs = target_dir.join(final_name);
    fs::rename(&source, &final_abs).map_err(|e| format!("restore failed: {e}"))?;
    let _ = fs::remove_dir_all(&trash_dir);
    asset_meta_for_path(root, &final_abs)
}

fn markdown_destination(rel_from_note: &str) -> String {
    format!("<{}>", rel_from_note.replace('>', "%3E"))
}

fn markdown_for_imported_asset(rel_from_note: &str, filename: &str, kind: &str) -> String {
    let dest = markdown_destination(rel_from_note);
    if kind == "image" {
        let (base, _) = split_ext(filename);
        format!("![{base}]({dest})")
    } else {
        format!("[{filename}]({dest})")
    }
}

fn posix_relative_from(note_dir: &str, target: &str) -> String {
    // POSIX relative path from note_dir to target (both vault-relative).
    let from: Vec<&str> = if note_dir.is_empty() { vec![] } else { note_dir.split('/').collect() };
    let to: Vec<&str> = target.split('/').collect();
    let mut i = 0;
    while i < from.len() && i < to.len() && from[i] == to[i] {
        i += 1;
    }
    let ups = from.len() - i;
    let mut parts: Vec<String> = std::iter::repeat("..".to_string()).take(ups).collect();
    parts.extend(to[i..].iter().map(|s| s.to_string()));
    parts.join("/")
}

/// `vault:import-files` — copy dropped files into the vault root, returning a
/// markdown snippet for each.
pub fn import_files(
    root: &Path,
    note_rel_path: &str,
    source_paths: &[String],
) -> Result<Vec<ImportedAsset>, String> {
    fs::create_dir_all(root).map_err(|e| format!("mkdir failed: {e}"))?;
    let note_dir = {
        let posix = to_posix(note_rel_path);
        match posix.rfind('/') {
            Some(idx) => posix[..idx].to_string(),
            None => String::new(),
        }
    };
    let mut imported = Vec::new();
    for source in source_paths {
        let source_abs = PathBuf::from(resolve_path(source));
        let Ok(md) = fs::metadata(&source_abs) else { continue };
        if !md.is_file() {
            continue;
        }
        let base = source_abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let final_name = unique_filename(root, &base);
        let dest = root.join(&final_name);
        fs::copy(&source_abs, &dest).map_err(|e| format!("copy failed: {e}"))?;
        let vault_rel = rel_of(root, &dest);
        let rel_from_note = posix_relative_from(&note_dir, &vault_rel);
        let kind = classify_imported_asset(&final_name);
        imported.push(ImportedAsset {
            name: final_name.clone(),
            path: vault_rel,
            markdown: markdown_for_imported_asset(&rel_from_note, &final_name, kind),
            kind: kind.to_string(),
        });
    }
    Ok(imported)
}

fn sanitize_pasted_base(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '%' | '*' | '?' | '"' | '<' | '>' | '|' | '[' | ']' | '#' | '^' => '-',
            c => c,
        })
        .collect();
    let mut collapsed = String::new();
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
    collapsed.trim().to_string()
}

/// `vault:import-pasted-image` — write clipboard image bytes to the vault root.
pub fn import_pasted_image(
    root: &Path,
    data: &[u8],
    mime_type: &str,
    suggested_name: Option<&str>,
) -> Result<ImportedAsset, String> {
    fs::create_dir_all(root).map_err(|e| format!("mkdir failed: {e}"))?;
    if data.is_empty() {
        return Err("Clipboard image is empty.".into());
    }
    let ext = pasted_image_extension(mime_type, suggested_name)
        .ok_or_else(|| "Clipboard item is not an image.".to_string())?;
    let raw_name = suggested_name
        .map(|n| Path::new(n).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
        .unwrap_or_default();
    let (raw_base, _) = split_ext(&raw_name);
    let base = sanitize_pasted_base(&raw_base);
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H%M%S").to_string();
    let final_base = if !base.is_empty() && base != "." && base != ".." {
        base
    } else {
        format!("Pasted Image {timestamp}")
    };
    let final_name = unique_filename(root, &format!("{final_base}{ext}"));
    let dest = root.join(&final_name);
    fs::write(&dest, data).map_err(|e| format!("write failed: {e}"))?;
    let vault_rel = rel_of(root, &dest);
    Ok(ImportedAsset {
        name: final_name,
        markdown: format!("![[{vault_rel}]]"),
        path: vault_rel,
        kind: "image".to_string(),
    })
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
    fn deleted_assets_list_purge_empty_roundtrip() {
        let v = vault();
        fs::write(v.path().join("inbox/pic.png"), b"png").unwrap();
        let deleted = delete_asset(v.path(), "inbox/pic.png").unwrap();
        assert!(deleted.deleted_at.is_some());

        let listed = list_deleted_assets(v.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "inbox/pic.png");
        assert_eq!(listed[0].undo_token, deleted.undo_token);

        purge_deleted_asset(v.path(), &deleted.undo_token).unwrap();
        assert!(list_deleted_assets(v.path()).is_empty());
        assert!(purge_deleted_asset(v.path(), "../../etc").is_err());

        fs::write(v.path().join("inbox/pic2.png"), b"png").unwrap();
        let _ = delete_asset(v.path(), "inbox/pic2.png").unwrap();
        empty_deleted_assets(v.path());
        assert!(list_deleted_assets(v.path()).is_empty());
    }

    #[test]
    fn list_excludes_md_and_internal() {
        let v = vault();
        fs::write(v.path().join("pic.png"), b"x").unwrap();
        fs::write(v.path().join("inbox/Note.md"), "n").unwrap();
        let assets = list_assets(v.path());
        assert!(assets.iter().any(|a| a.path == "pic.png" && a.kind == "image"));
        assert!(!assets.iter().any(|a| a.path.ends_with(".md")));
    }

    #[test]
    fn delete_then_restore_roundtrips() {
        let v = vault();
        fs::write(v.path().join("doc.pdf"), b"pdf").unwrap();
        let deleted = delete_asset(v.path(), "doc.pdf").unwrap();
        assert!(!v.path().join("doc.pdf").exists());
        let restored = restore_deleted_asset(v.path(), &deleted).unwrap();
        assert_eq!(restored.path, "doc.pdf");
        assert!(v.path().join("doc.pdf").exists());
    }

    #[test]
    fn pasted_image_writes_and_returns_embed() {
        let v = vault();
        let asset = import_pasted_image(v.path(), b"\x89PNG", "image/png", None).unwrap();
        assert!(asset.name.ends_with(".png"));
        assert_eq!(asset.markdown, format!("![[{}]]", asset.path));
        assert!(v.path().join(&asset.name).exists());
    }

    #[test]
    fn import_files_makes_relative_markdown() {
        let v = vault();
        // Source lives OUTSIDE the vault so the copied name is preserved.
        let ext_dir = tempfile::tempdir().unwrap();
        let src = ext_dir.path().join("external.png");
        fs::write(&src, b"x").unwrap();
        let imported = import_files(
            v.path(),
            "inbox/Note.md",
            &[src.to_string_lossy().to_string()],
        )
        .unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].path, "external.png");
        // Note is in inbox/, asset at root → "../external.png".
        assert!(imported[0].markdown.contains("../external.png"), "{}", imported[0].markdown);
    }
}
