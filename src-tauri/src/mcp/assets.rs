//! Attachments for the MCP tools (ZenNotes/tui#34), ported from the asset
//! half of upstream apps/desktop/src/mcp/vault-ops.ts and the tools in
//! asset-tools.ts.
//!
//! Every tool finds its file in the vault's asset listing, whatever the
//! scope: the listing leaves out notes, hidden and internal files, database
//! folders and links to files, so nothing else can be named through these
//! tools. Deleting is the soft delete the app's Trash can undo: the file moves
//! into `.zennotes/deleted-assets/<token>/` beside a `.zn-deleted.json`
//! record of where it was, the same store `crate::vault::assets` (the app's
//! Assets and Trash views) lists, restores and purges. Purging is left out on
//! purpose, as upstream does: it cannot be undone and the user can do it in
//! the app.
//!
//! The operations here are upstream's MCP copies, not the app's: their own
//! error wording, never-overwrite placement (`renameNoClobber`) and link
//! rewriting after a rename or move.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::asset_links::{rewrite_asset_references, AssetRef, NoteRef};
use super::backend::Backend;
use super::ops::{
    self, abs_root, basename, extname, node_error, ms, re, rel_path, resolve_safe, stem_of, AssetMeta, NoteMeta, JS_WS,
};
use super::scope::{fold, go_quote, is_inside_folded, js_trim};
use super::server::{optional_boolean, require_string, Args};
use crate::vault::layout::{ASSETS_DIR, ATTACHMENTS_DIRS, INTERNAL_VAULT_DIR};

const DELETED_ASSETS_DIR: &str = "deleted-assets";
const DELETED_ASSET_META: &str = ".zn-deleted.json";
/// A tool call carries the file as base64 inside one JSON-RPC message.
const MAX_UPLOAD_ASSET_BYTES: usize = 16 * 1024 * 1024;
/// Past this a client's context limits refuse the result anyway.
const MAX_READ_ASSET_BYTES: u64 = 5 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const DATABASE_SIDECAR_SUFFIX: &str = ".base.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedAsset {
    pub path: String,
    pub name: String,
    pub undo_token: String,
    pub deleted_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedAsset {
    pub name: String,
    pub path: String,
    pub markdown: String,
    pub kind: String,
}

/* ---------- Listing --------------------------------------------------------- */

static ATOMIC_WRITE_TEMP_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\.[0-9]+\.[0-9]{13,}\.tmp$"));

/// A temp file an atomic write leaves while it works (`isAtomicWriteTempPath`).
pub fn is_atomic_write_temp_path(p: &str) -> bool {
    ATOMIC_WRITE_TEMP_RE.is_match(basename(p)).unwrap_or(false)
}

fn is_form_dir_name(name: &str) -> bool {
    name.to_lowercase().ends_with(".base")
}

fn asset_meta(root: &Path, abs: &Path, md: &fs::Metadata) -> AssetMeta {
    AssetMeta {
        path: rel_path(root, abs),
        name: abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        size: md.len(),
        updated_at: ms(md.modified()),
        links_not_updated: None,
    }
}

fn asset_meta_for_path(root: &Path, abs: &Path) -> Result<AssetMeta, String> {
    let md = fs::metadata(abs).map_err(|e| node_error(&e, "stat", abs))?;
    Ok(asset_meta(root, abs, &md))
}

fn walk_assets(root: &Path, dir: &Path, out: &mut Vec<AssetMeta>) -> Result<(), String> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) => {
            let quiet = matches!(e.raw_os_error(), Some(1) | Some(2) | Some(13)) || e.kind() == std::io::ErrorKind::NotFound;
            if dir != root || quiet {
                return Ok(());
            }
            return Err(node_error(&e, "scandir", dir));
        }
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || is_atomic_write_temp_path(&name) {
            continue;
        }
        let full = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if !is_form_dir_name(&name) {
                walk_assets(root, &full, out)?;
            }
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let lower = name.to_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".excalidraw") {
            continue;
        }
        // Gone since the directory was read: left out.
        if let Ok(md) = fs::symlink_metadata(&full) {
            out.push(asset_meta(root, &full, &md));
        }
    }
    Ok(())
}

/// Every loose file in the vault, as the server and the app's Assets view
/// list them. Notes, drawings, database folders, hidden and internal files
/// and symbolic links are not attachments, but a link at an attachment
/// folder's own name is walked, as the app walks it. Newest first.
pub fn list_assets(root: &Path) -> Result<Vec<AssetMeta>, String> {
    let root = abs_root(root);
    let mut out = Vec::new();
    walk_assets(&root, &root, &mut out)?;
    for dir in ATTACHMENTS_DIRS {
        let full = root.join(dir);
        let linked = fs::symlink_metadata(&full).is_ok_and(|m| m.file_type().is_symlink());
        if linked && fs::metadata(&full).is_ok_and(|m| m.is_dir()) {
            walk_assets(&root, &full, &mut out)?;
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

/* ---------- Helpers ----------------------------------------------------------- */

static LEADING_TRAILING_SLASHES: LazyLock<Regex> = LazyLock::new(|| re(r"^/+|/+$"));

fn trim_vault_relative(rel: &str) -> String {
    let posix = js_trim(rel).replace('\\', "/");
    LEADING_TRAILING_SLASHES.replace_all(&posix, "").into_owned()
}

fn assert_asset_file(root: &Path, rel: &str) -> Result<(String, PathBuf), String> {
    let normalized = trim_vault_relative(rel);
    if normalized.is_empty() {
        return Err("asset path is required".into());
    }
    if normalized.split('/').any(|s| s == INTERNAL_VAULT_DIR) {
        return Err("cannot modify internal ZenNotes files".into());
    }
    if extname(&normalized).to_lowercase() == ".md" {
        return Err("use note actions to modify markdown notes".into());
    }
    let abs = resolve_safe(root, &normalized)?;
    let md = fs::metadata(&abs).map_err(|e| node_error(&e, "stat", &abs))?;
    if md.is_dir() {
        return Err("asset path is not a file".into());
    }
    Ok((rel_path(root, &abs), abs))
}

fn clean_asset_filename(name: &str) -> Result<String, String> {
    let raw = js_trim(name);
    if raw.contains('/') || raw.contains('\\') {
        return Err("use only a file name".into());
    }
    let trimmed = basename(raw);
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err("asset name is required".into());
    }
    if extname(trimmed).to_lowercase() == ".md" {
        return Err("use note actions for markdown notes".into());
    }
    Ok(trimmed.to_string())
}

/// Why placing a file at a name failed: taken (try the next number), turned
/// down by the caller's filter, or anything else.
enum PlaceError {
    Taken,
    Refused,
    Other(String),
}

impl PlaceError {
    fn from_io(e: std::io::Error, syscall: &str, p: &Path) -> Self {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            PlaceError::Taken
        } else {
            PlaceError::Other(node_error(&e, syscall, p))
        }
    }
}

const ASSET_DESTINATION_REFUSED: &str = "\u{0}asset destination refused";

fn unique_asset_path(dir: &Path, filename: &str) -> Result<PathBuf, String> {
    let ext = extname(filename);
    let stem = stem_of(filename);
    let mut candidate = dir.join(filename);
    let mut n = 2;
    loop {
        match fs::symlink_metadata(&candidate) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(e) => return Err(node_error(&e, "lstat", &candidate)),
            Ok(_) => {}
        }
        candidate = dir.join(format!("{stem} {n}{ext}"));
        n += 1;
    }
}

/// Puts a file at the first free name in `dir`, taking the next number if the
/// name goes between the check and the write.
fn place_at_free_name(
    dir: &Path,
    filename: &str,
    mut place: impl FnMut(&Path) -> Result<(), PlaceError>,
) -> Result<PathBuf, String> {
    for _ in 0..100 {
        let candidate = unique_asset_path(dir, filename)?;
        match place(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(PlaceError::Taken) => continue,
            Err(PlaceError::Refused) => return Err(ASSET_DESTINATION_REFUSED.into()),
            Err(PlaceError::Other(message)) => return Err(message),
        }
    }
    Err(format!("no free name for {filename}"))
}

/// Moves a file to a name only if nothing is there, against any other
/// program too: a hard link fails on a taken name, where a rename replaces
/// what is there. A volume without hard links gets the name held by an empty
/// file first.
fn rename_no_clobber(from: &Path, to: &Path) -> Result<(), PlaceError> {
    match fs::hard_link(from, to) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists || e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PlaceError::from_io(e, "link", from));
        }
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV: copy, never over a taken name.
            let copied = copy_exclusive(from, to);
            if let Err(err) = copied {
                if !matches!(err, PlaceError::Taken) {
                    let _ = fs::remove_file(to);
                }
                return Err(err);
            }
        }
        Err(_) => {
            fs::OpenOptions::new().write(true).create_new(true).open(to).map_err(|e| PlaceError::from_io(e, "open", to))?;
            if let Err(e) = fs::rename(from, to) {
                let _ = fs::remove_file(to);
                return Err(PlaceError::Other(node_error(&e, "rename", from)));
            }
            return Ok(());
        }
    }
    if let Err(e) = fs::remove_file(from) {
        let _ = fs::remove_file(to);
        return Err(PlaceError::Other(node_error(&e, "unlink", from)));
    }
    Ok(())
}

/// `fs.copyFile(from, to, COPYFILE_EXCL)`.
fn copy_exclusive(from: &Path, to: &Path) -> Result<(), PlaceError> {
    let mut source = fs::File::open(from).map_err(|e| PlaceError::from_io(e, "copyfile", from))?;
    let mut dest =
        fs::OpenOptions::new().write(true).create_new(true).open(to).map_err(|e| PlaceError::from_io(e, "copyfile", to))?;
    if let Err(e) = std::io::copy(&mut source, &mut dest) {
        drop(dest);
        let _ = fs::remove_file(to);
        return Err(PlaceError::Other(node_error(&e, "copyfile", from)));
    }
    Ok(())
}

/// Re-target the notes that reference a renamed or moved asset, skipping the
/// trash and whatever `rewrite_in` turns down. The snapshots are from before
/// the change, so references still resolve to the asset under its old path.
/// Returns the notes it could not read or write, or whose wikilinks cannot
/// hold the new path.
fn rewrite_asset_links(
    root: &Path,
    notes_before: &[NoteMeta],
    assets_before: &[AssetMeta],
    old_rel: &str,
    new_rel: &str,
    rewrite_in: Option<&dyn Fn(&str) -> bool>,
) -> Vec<String> {
    let mut failed = Vec::new();
    let notes: Vec<&NoteMeta> = notes_before.iter().filter(|n| n.folder != "trash").collect();
    let nameable: Vec<&NoteMeta> = match rewrite_in {
        Some(keep) => notes.into_iter().filter(|n| keep(&n.path)).collect(),
        None => notes,
    };
    let note_refs: Vec<NoteRef> =
        nameable.iter().map(|n| NoteRef { path: &n.path, title: &n.title, folder: &n.folder }).collect();
    let asset_refs: Vec<AssetRef> = assets_before.iter().map(|a| AssetRef { path: &a.path }).collect();
    for note in &nameable {
        let Ok(content) = ops::read_note(root, &note.path) else {
            failed.push(note.path.clone());
            continue;
        };
        let rewrite = rewrite_asset_references(
            &content.body,
            &asset_refs,
            &note.path,
            old_rel,
            new_rel,
            &note_refs,
            rewrite_in.is_some(),
        );
        // One note that cannot be written leaves the rest to do.
        let written = rewrite.changed == 0 || ops::write_note(root, &note.path, &rewrite.body).is_ok();
        if !written || rewrite.stranded {
            failed.push(note.path.clone());
        }
    }
    failed
}

/// A moved or renamed asset, saying which notes may still use the old name.
fn with_link_rewrite(mut meta: AssetMeta, failed: Vec<String>) -> AssetMeta {
    if !failed.is_empty() {
        meta.links_not_updated = Some(failed);
    }
    meta
}

/* ---------- Operations -------------------------------------------------------- */

pub fn read_asset(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let (_, abs) = assert_asset_file(root, rel)?;
    fs::read(&abs).map_err(|e| node_error(&e, "open", &abs))
}

const IMAGE_EXTENSIONS: &[&str] = &[".apng", ".avif", ".gif", ".jpeg", ".jpg", ".png", ".svg", ".webp"];
const AUDIO_EXTENSIONS: &[&str] = &[".aac", ".flac", ".m4a", ".mp3", ".ogg", ".wav"];
const VIDEO_EXTENSIONS: &[&str] = &[".m4v", ".mov", ".mp4", ".ogv", ".webm"];

fn classify_asset(filename: &str) -> &'static str {
    let ext = extname(filename).to_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        "image"
    } else if ext == ".pdf" {
        "pdf"
    } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        "audio"
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        "video"
    } else {
        "file"
    }
}

/// The one link rule of the app and the server: an image, PDF, audio or video
/// file as an embed, anything else as a markdown link, by vault path.
fn markdown_for_asset(rel: &str, name: &str, kind: &str) -> String {
    if kind != "file" {
        return format!("![[{rel}]]");
    }
    format!("[{name}](<{}>)", rel.replace('>', "%3E"))
}

static RESERVED_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:con|prn|aux|nul|conin\$|conout\$|com[1-9\x{b9}\x{b2}\x{b3}]|lpt[1-9\x{b9}\x{b2}\x{b3}]) *(?:\.|$)")
});

/// A device name Windows keeps, with or without an extension.
pub fn is_reserved_file_name(name: &str) -> bool {
    RESERVED_NAME_RE.is_match(name).unwrap_or(false)
}

static IMPORT_UNSAFE_RE: LazyLock<Regex> = LazyLock::new(|| re(r#"[\\/:%\x00-\x1f*?"<>|\[\]#^]"#));
static WS_RUN_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!("{JS_WS}+")));
static TRAILING_DOTS_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[. ]+$"));

/// A name that cannot terminate or retarget the link written for it.
pub fn imported_asset_filename(filename: &str) -> String {
    let leaf = filename.rsplit('/').next().unwrap_or(filename);
    let replaced = IMPORT_UNSAFE_RE.replace_all(leaf, "-");
    let collapsed = WS_RUN_RE.replace_all(&replaced, " ");
    // Another system reads a trailing dot as no dot at all.
    let safe = TRAILING_DOTS_RE.replace(js_trim(&collapsed), "").into_owned();
    if is_reserved_file_name(&safe) {
        return format!("_{safe}");
    }
    if safe.is_empty() || safe == "." || safe == ".." { "file".into() } else { safe }
}

/// Add a file to the unified `assets/` folder, never overwriting: a taken
/// name gets a number.
pub fn import_asset(root: &Path, filename: &str, bytes: &[u8]) -> Result<ImportedAsset, String> {
    let assets_dir = resolve_safe(root, ASSETS_DIR)?;
    fs::create_dir_all(&assets_dir).map_err(|e| node_error(&e, "mkdir", &assets_dir))?;
    let abs = place_at_free_name(&assets_dir, &imported_asset_filename(filename), |candidate| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(candidate)
            .map_err(|e| PlaceError::from_io(e, "open", candidate))?;
        if let Err(e) = file.write_all(bytes) {
            // A full disk leaves part of the file.
            drop(file);
            let _ = fs::remove_file(candidate);
            return Err(PlaceError::Other(node_error(&e, "write", candidate)));
        }
        Ok(())
    })?;
    let name = abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let rel = rel_path(root, &abs);
    let kind = classify_asset(&name);
    Ok(ImportedAsset { markdown: markdown_for_asset(&rel, &name, kind), name, path: rel, kind: kind.into() })
}

fn rename_err(e: std::io::Error, from: &Path) -> String {
    node_error(&e, "rename", from)
}

pub fn rename_asset(
    root: &Path,
    rel: &str,
    next_name: &str,
    rewrite_in: Option<&dyn Fn(&str) -> bool>,
) -> Result<AssetMeta, String> {
    // The link rewrite reads every note.
    let notes_before = ops::list_notes(root);
    let assets_before = list_assets(root)?;
    let (old_rel, source) = assert_asset_file(root, rel)?;
    let clean = clean_asset_filename(next_name)?;
    let dir = source.parent().map(Path::to_path_buf).unwrap_or_else(|| abs_root(root));
    let dest = dir.join(&clean);
    if dest != source {
        let already_there = || format!("an asset named {} already exists in this folder", go_quote(&clean));
        // Not stat, which follows a link: anything at the name means it is taken.
        let taken = match fs::symlink_metadata(&dest) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(node_error(&e, "lstat", &dest)),
        };
        let source_name = source.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if taken {
            // Only the file itself may sit at the destination: a change of case
            // on a case-insensitive volume, which goes through a scratch name.
            let same_name = super::scope::nfc(&clean).to_lowercase() == super::scope::nfc(&source_name).to_lowercase();
            let listed: Vec<String> = fs::read_dir(&dir)
                .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            if !same_name || (listed.contains(&clean) && listed.contains(&source_name)) {
                return Err(already_there());
            }
            let same = matches!(
                (crate::vault::sidecars::file_identity(&source), crate::vault::sidecars::file_identity(&dest)),
                (Ok(a), Ok(b)) if a == b
            );
            if !same {
                return Err(already_there());
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let tmp = PathBuf::from(format!("{}.{}.{stamp}000.tmp", source.to_string_lossy(), std::process::id()));
            crate::vault::atomic::rename_with_retry(&source, &tmp).map_err(|e| rename_err(e, &source))?;
            if let Err(e) = crate::vault::atomic::rename_with_retry(&tmp, &dest) {
                let _ = crate::vault::atomic::rename_with_retry(&tmp, &source);
                return Err(rename_err(e, &tmp));
            }
            // Two hard links of one file: that rename does nothing, and leaves it.
            let _ = fs::remove_file(&tmp);
        } else {
            rename_no_clobber(&source, &dest).map_err(|e| match e {
                PlaceError::Taken => already_there(),
                PlaceError::Refused => ASSET_DESTINATION_REFUSED.into(),
                PlaceError::Other(m) => m,
            })?;
        }
    }
    let meta = asset_meta_for_path(root, &dest)?;
    if meta.path == old_rel {
        return Ok(meta);
    }
    let failed = rewrite_asset_links(root, &notes_before, &assets_before, &old_rel, &meta.path, rewrite_in);
    Ok(with_link_rewrite(meta, failed))
}

/// Whether a folder about to be made means the same folder on every system.
fn holds_everywhere(seg: &str) -> bool {
    !(seg.ends_with('.') || seg.ends_with(' ') || seg.contains(':'))
}

/// A vault-relative folder with each segment that exists written as its
/// parent lists it, so a note reads like the listing. None for a segment that
/// is not a real folder under a name its parent lists, or a new one that
/// would not hold on every system.
pub fn folders_as_listed(root: &Path, rel: &str) -> Option<String> {
    let segments: Vec<&str> = rel.split('/').collect();
    let mut out: Vec<String> = Vec::new();
    let mut walk = abs_root(root);
    for (at, seg) in segments.iter().enumerate() {
        match fs::symlink_metadata(walk.join(seg)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let fresh = &segments[at..];
                if !fresh.iter().all(|s| holds_everywhere(s)) {
                    return None;
                }
                out.extend(fresh.iter().map(|s| s.to_string()));
                return Some(out.join("/"));
            }
            Err(_) => return None,
            Ok(info) if !info.is_dir() => return None,
            Ok(_) => {}
        }
        let listed: Vec<String> = fs::read_dir(&walk)
            .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        let folded = fold(seg);
        let name = if listed.iter().any(|n| n == seg) {
            seg.to_string()
        } else {
            listed.into_iter().find(|n| fold(n) == folded)?
        };
        walk.push(&name);
        out.push(name);
    }
    Some(out.join("/"))
}

/// Move an asset into `target_dir`, creating it; a taken name gets a number.
/// A filter that turns the landing down fails with the private
/// `ASSET_DESTINATION_REFUSED` text, which the scope maps to its refusal.
pub fn move_asset(
    root: &Path,
    rel: &str,
    target_dir: &str,
    rewrite_in: Option<&dyn Fn(&str) -> bool>,
) -> Result<AssetMeta, String> {
    let notes_before = ops::list_notes(root);
    let assets_before = list_assets(root)?;
    let (old_rel, source) = assert_asset_file(root, rel)?;
    let normalized = trim_vault_relative(target_dir);
    if normalized.split('/').any(|s| s == INTERNAL_VAULT_DIR) {
        return Err("cannot move assets into internal ZenNotes files".into());
    }
    let dest_dir = resolve_safe(root, if normalized.is_empty() { ASSETS_DIR } else { &normalized })?;
    let source_name = source.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // A folder that is not there yet has no taken names, so this is where the
    // file would land. A refusal leaves no empty folder behind.
    let landing = rel_path(root, &dest_dir.join(&source_name));
    if let Some(keep) = rewrite_in {
        if !keep(&landing) && fs::metadata(&dest_dir).is_err() {
            return Err(ASSET_DESTINATION_REFUSED.into());
        }
    }
    fs::create_dir_all(&dest_dir).map_err(|e| node_error(&e, "mkdir", &dest_dir))?;
    // By identity: a folder spelled another way on a case-insensitive volume
    // is this one too, and a move would only give the file a number.
    let source_dir = source.parent().map(Path::to_path_buf).unwrap_or_else(|| abs_root(root));
    let same_dir = matches!(
        (crate::vault::sidecars::file_identity(&dest_dir), crate::vault::sidecars::file_identity(&source_dir)),
        (Ok(a), Ok(b)) if a == b
    );
    if same_dir {
        return asset_meta_for_path(root, &source);
    }
    // A filter also has to accept where the file lands, which only this knows.
    let dest = place_at_free_name(&dest_dir, &source_name, |candidate| {
        if let Some(keep) = rewrite_in {
            if !keep(&rel_path(root, candidate)) {
                return Err(PlaceError::Refused);
            }
        }
        rename_no_clobber(&source, candidate)
    })?;
    let meta = asset_meta_for_path(root, &dest)?;
    if meta.path == old_rel {
        return Ok(meta);
    }
    let failed = rewrite_asset_links(root, &notes_before, &assets_before, &old_rel, &meta.path, rewrite_in);
    Ok(with_link_rewrite(meta, failed))
}

pub fn is_destination_refused(message: &str) -> bool {
    message == ASSET_DESTINATION_REFUSED
}

/// Copy an asset next to itself as `<name> copy<ext>`.
pub fn duplicate_asset(root: &Path, rel: &str) -> Result<AssetMeta, String> {
    let (_, source) = assert_asset_file(root, rel)?;
    let name = source.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = source.parent().map(Path::to_path_buf).unwrap_or_else(|| abs_root(root));
    let copy_name = format!("{} copy{}", stem_of(&name), extname(&name));
    let dest = place_at_free_name(&dir, &copy_name, |candidate| copy_exclusive(&source, candidate))?;
    asset_meta_for_path(root, &dest)
}

fn is_token(token: &str) -> bool {
    token.len() == 36 && token.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn clean_deleted_asset_path(rel: &str) -> Result<String, String> {
    let normalized = trim_vault_relative(rel);
    if normalized.is_empty() {
        return Err("deleted asset path is required".into());
    }
    // A stored record decides where a file lands, so it gets the same reading
    // as a path a client sends: nothing hidden or internal, in any spelling.
    for seg in normalized.split('/') {
        if seg.is_empty() || seg.starts_with('.') || seg.contains('\0') {
            return Err("cannot restore internal ZenNotes files".into());
        }
    }
    if extname(&normalized).to_lowercase() == ".md" {
        return Err("use note actions to restore markdown notes".into());
    }
    Ok(normalized)
}

/// `new Date().toISOString()`.
fn iso_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Soft delete: the file moves into `.zennotes/deleted-assets/<token>/`
/// beside a record of where it was, the store the app's Trash lists.
/// Metadata first, file last, so a failure anywhere leaves the asset in the
/// vault.
pub fn delete_asset(root: &Path, rel: &str) -> Result<DeletedAsset, String> {
    let (rel, source) = assert_asset_file(root, rel)?;
    let undo_token = uuid::Uuid::new_v4().to_string();
    let trash_dir = resolve_safe(root, &format!("{INTERNAL_VAULT_DIR}/{DELETED_ASSETS_DIR}/{undo_token}"))?;
    fs::create_dir_all(&trash_dir).map_err(|e| node_error(&e, "mkdir", &trash_dir))?;
    let name = source.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let deleted_at = iso_now();
    let record = json!({ "path": rel, "name": name, "deletedAt": deleted_at });
    let written = fs::write(trash_dir.join(DELETED_ASSET_META), serde_json::to_string_pretty(&record).unwrap_or_default())
        .map_err(|e| node_error(&e, "open", &trash_dir.join(DELETED_ASSET_META)))
        .and_then(|()| fs::rename(&source, trash_dir.join(&name)).map_err(|e| rename_err(e, &source)));
    if let Err(message) = written {
        let _ = fs::remove_dir_all(&trash_dir);
        return Err(message);
    }
    Ok(DeletedAsset { path: rel, name, undo_token, deleted_at })
}

fn read_deleted_entry(store: &Path, token: &str) -> Option<DeletedAsset> {
    let raw = fs::read_to_string(store.join(token).join(DELETED_ASSET_META)).ok()?;
    let meta: Value = serde_json::from_str(&raw).ok()?;
    let path = meta.get("path")?.as_str()?;
    let name = meta.get("name")?.as_str()?;
    if path.is_empty() || name.is_empty() {
        return None;
    }
    let clean = clean_asset_filename(name).ok()?;
    if clean == DELETED_ASSET_META {
        return None;
    }
    clean_deleted_asset_path(path).ok()?;
    if !fs::metadata(store.join(token).join(&clean)).ok()?.is_file() {
        return None;
    }
    Some(DeletedAsset {
        path: path.to_string(),
        name: name.to_string(),
        undo_token: token.to_string(),
        deleted_at: meta.get("deletedAt").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

/// What can be restored, newest first. An entry without a readable record or
/// its file is skipped.
pub fn list_deleted_assets(root: &Path) -> Result<Vec<DeletedAsset>, String> {
    let store = resolve_safe(root, &format!("{INTERNAL_VAULT_DIR}/{DELETED_ASSETS_DIR}"))?;
    let mut tokens: Vec<String> = match fs::read_dir(&store) {
        Ok(read) => read.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(node_error(&e, "scandir", &store)),
    };
    tokens.sort();
    let mut out: Vec<DeletedAsset> =
        tokens.iter().filter(|t| is_token(t)).filter_map(|t| read_deleted_entry(&store, t)).collect();
    out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    Ok(out)
}

/// Bring a deleted asset back. Only the token comes from the caller; the
/// stored record decides what is restored and where.
pub fn restore_deleted_asset(root: &Path, undo_token: &str) -> Result<AssetMeta, String> {
    if !is_token(undo_token) {
        return Err("deleted asset restore token is invalid".into());
    }
    let trash_dir = resolve_safe(root, &format!("{INTERNAL_VAULT_DIR}/{DELETED_ASSETS_DIR}/{undo_token}"))?;
    let raw = fs::read_to_string(trash_dir.join(DELETED_ASSET_META)).map_err(|_| "deleted asset entry not found".to_string())?;
    let stored: Value = serde_json::from_str(&raw).map_err(|_| "deleted asset entry is unreadable".to_string())?;
    let (Some(path), Some(name)) = (stored.get("path").and_then(Value::as_str), stored.get("name").and_then(Value::as_str)) else {
        return Err("deleted asset entry is unreadable".into());
    };
    let target_rel = clean_deleted_asset_path(path)?;
    let name = clean_asset_filename(name)?;
    if name == DELETED_ASSET_META {
        return Err("deleted asset entry is unreadable".into());
    }
    let target_abs = resolve_safe(root, &target_rel)?;
    let target_dir = target_abs.parent().map(Path::to_path_buf).unwrap_or_else(|| abs_root(root));
    fs::create_dir_all(&target_dir).map_err(|e| node_error(&e, "mkdir", &target_dir))?;
    let target_name = target_abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stored_file = trash_dir.join(&name);
    let final_abs = place_at_free_name(&target_dir, &target_name, |candidate| rename_no_clobber(&stored_file, candidate))?;
    let _ = fs::remove_dir_all(&trash_dir);
    asset_meta_for_path(root, &final_abs)
}

/// The vault's own templates in `.zennotes/templates`, as (sourcePath, raw).
pub fn list_template_files(root: &Path) -> Result<Vec<(String, String)>, String> {
    let dir = abs_root(root).join(INTERNAL_VAULT_DIR).join("templates");
    let read = match fs::read_dir(&dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(node_error(&e, "scandir", &dir)),
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().is_ok_and(|t| t.is_file()) || name.starts_with('.') || extname(&name).to_lowercase() != ".md" {
            continue;
        }
        // A template that cannot be read is not offered.
        if let Ok(raw) = ops::read_text(&entry.path()) {
            out.push((format!("{INTERNAL_VAULT_DIR}/templates/{name}"), raw));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/* ---------- Tool support -------------------------------------------------------- */

pub fn asset_not_found(raw: &str) -> String {
    format!("Asset not found: {raw}")
}

pub fn deleted_asset_not_found(token: &str) -> String {
    format!("Deleted asset not found: {token}")
}

/// A client's path in vault-relative POSIX form, or None: `.` and `..`
/// segments and hidden names are never resolved.
pub fn clean_request(raw: &str) -> Option<String> {
    let posix = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for seg in posix.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg == "." || seg == ".." || seg.starts_with('.') || seg.contains('\0') {
            return None;
        }
        parts.push(seg);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A client's path in an asset listing: exact, else the one path equal after
/// case and accent folding, so the vault's spelling is what gets used.
pub fn match_asset<'a>(all: &'a [AssetMeta], raw: &str) -> Option<&'a AssetMeta> {
    let clean = clean_request(raw)?;
    let folded = fold(&clean);
    let mut found = None;
    let mut count = 0;
    for asset in all {
        if asset.path == clean {
            return Some(asset);
        }
        if fold(&asset.path) == folded {
            found = Some(asset);
            count += 1;
        }
    }
    if count == 1 { found } else { None }
}

fn find_asset(backend: &dyn Backend, raw: &str) -> Result<AssetMeta, String> {
    let all = backend.list_assets()?;
    match_asset(&all, raw).cloned().ok_or_else(|| asset_not_found(raw))
}

fn is_database_sidecar_path(rel: &str) -> bool {
    rel.to_lowercase().ends_with(&format!(".csv{DATABASE_SIDECAR_SUFFIX}"))
}

/// `formDirContaining`: the `.base` folder a path sits in, if any.
pub fn form_dir_containing(rel: &str) -> Option<String> {
    let posix = rel.replace('\\', "/");
    let parts: Vec<&str> = posix.split('/').collect();
    parts.iter().position(|p| p.to_lowercase().ends_with(".base")).map(|i| parts[..=i].join("/"))
}

/// Keeps these tools off a loose CSV database and its schema file: they move
/// as a pair, which only the database code knows how to do.
fn refuse_database_file(meta: &AssetMeta, backend: &dyn Backend) -> Result<(), String> {
    let refusal = "That file belongs to a ZenNotes database. Change it from the app.".to_string();
    if is_database_sidecar_path(&meta.path) {
        return Err(refusal);
    }
    if !meta.path.to_lowercase().ends_with(".csv") || form_dir_containing(&meta.path).is_some() {
        return Ok(());
    }
    // The whole listing: a pair stays a pair when a scope hides half of it.
    let all = match backend.scoped() {
        Some(scoped) => scoped.inner.list_assets()?,
        None => backend.list_assets()?,
    };
    let sidecar = fold(&format!("{}{DATABASE_SIDECAR_SUFFIX}", meta.path));
    if all.iter().any(|a| fold(&a.path) == sidecar) {
        return Err(refusal);
    }
    Ok(())
}

/// What a loose CSV keeps beside it that is not the user's: its schema file
/// and the backups of either. The app's Assets view leaves these out.
fn is_loose_csv_internal(rel: &str) -> bool {
    let lower = rel.to_lowercase();
    is_database_sidecar_path(&lower) || lower.ends_with(".csv.bak") || lower.ends_with(&format!(".csv{DATABASE_SIDECAR_SUFFIX}.bak"))
}

/// Refuses a name the asset listing would not show afterwards, since a file
/// the listing cannot see is one no tool can reach again.
fn check_asset_name(name: &str) -> Result<(), String> {
    let trimmed = js_trim(name);
    let lower = trimmed.to_lowercase();
    let bare = TRAILING_DOTS_RE.replace(&lower, "");
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err("A file name is required.".into());
    }
    if trimmed.contains(['/', '\\', '\0']) {
        return Err("Use only a file name, with no folder in it.".into());
    }
    if trimmed.starts_with('.') {
        return Err("A file name cannot start with a dot.".into());
    }
    if bare.ends_with(".md") {
        return Err("Notes are not attachments: use create_note or rename_note.".into());
    }
    if bare.ends_with(".excalidraw") || is_atomic_write_temp_path(trimmed) || is_loose_csv_internal(&bare) {
        return Err("That kind of file cannot be managed as an attachment.".into());
    }
    Ok(())
}

static NAME_BREAKS_LINKS: LazyLock<Regex> = LazyLock::new(|| re(r#"[#?|\[\]^%:*"<>`\x00-\x1f\x7f]|[. ]$"#));

fn check_asset_new_name(name: &str) -> Result<(), String> {
    check_asset_name(name)?;
    let trimmed = js_trim(name);
    let before_colon = trimmed.split(':').next().unwrap_or("");
    if TRAILING_DOTS_RE.replace(before_colon, "").to_lowercase().ends_with(".md") {
        return Err("Notes are not attachments: use create_note or rename_note.".into());
    }
    if is_reserved_file_name(trimmed) {
        return Err("That name is reserved on Windows, so choose another.".into());
    }
    if NAME_BREAKS_LINKS.is_match(trimmed).unwrap_or(false) {
        return Err("A file name cannot contain # ? | [ ] ^ % : * \" < > ` or a control character, or end with a dot, since links to it would break.".into());
    }
    Ok(())
}

/// Refuses a destination whatever the scope: anything that is not a plain
/// vault-relative folder, a database's insides, and the trash, whose contents
/// empty_trash and the app destroy. Under a scope nothing here looks at the
/// disk; what is there is for the scope to judge.
fn check_asset_target_dir(backend: &dyn Backend, raw: &str) -> Result<String, String> {
    let refused = || "This assistant cannot put files there.".to_string();
    let Some(clean) = clean_request(raw) else { return Err(refused()) };
    // The move trims what it is given, so this has to be what it will use.
    if js_trim(raw) != raw {
        return Err(refused());
    }
    if clean.split('/').any(|s| s.to_lowercase().ends_with(".base")) {
        return Err(refused());
    }
    if is_inside_folded(&clean, "trash") {
        return Err(match backend.scoped() {
            Some(scoped) => scoped.destination_refusal(),
            None => refused(),
        });
    }
    if backend.scoped().is_some() {
        return Ok(clean);
    }
    folders_as_listed(backend.root(), &clean).ok_or_else(refused)
}

/// Reads what a client plausibly sends: standard or URL-safe, padded or not,
/// wrapped lines, a data: URL. It refuses by length before it decodes.
pub fn decode_asset_base64(raw: &str) -> Result<Vec<u8>, String> {
    let mut text = raw;
    if let Some(rest) = raw.strip_prefix("data:") {
        let comma = rest.find(',').map(|i| i + 5);
        match comma {
            Some(i) if raw[..i].ends_with(";base64") => text = &raw[i + 1..],
            _ => return Err("content_base64 is a data: URL that is not base64.".into()),
        }
    }
    let too_large = || {
        format!(
            "upload_asset takes files up to {} MiB; this one is larger. Add it from the app or copy it into the vault instead.",
            MAX_UPLOAD_ASSET_BYTES as u64 / MIB
        )
    };
    let encoded_limit = MAX_UPLOAD_ASSET_BYTES.div_ceil(3) * 4;
    let units = if text.is_ascii() { text.len() } else { text.encode_utf16().count() };
    if units > encoded_limit * 2 {
        return Err(too_large());
    }
    let mut cleaned: String =
        text.chars().filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n')).map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        }).collect();
    while cleaned.ends_with('=') {
        cleaned.pop();
    }
    let len = cleaned.encode_utf16().count();
    if len * 6 / 8 > MAX_UPLOAD_ASSET_BYTES {
        return Err(too_large());
    }
    let alphabet = cleaned.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    if !alphabet || len % 4 == 1 {
        return Err("content_base64 is not valid base64.".into());
    }
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone),
    );
    let data = engine.decode(cleaned.as_bytes()).map_err(|_| "content_base64 is not valid base64.".to_string())?;
    if data.is_empty() {
        return Err("content_base64 is empty.".into());
    }
    Ok(data)
}

/// A read_asset answer: a line of facts, then the file as the MCP content
/// type a client can use. A model sees an image, reads text as text, and
/// anything else arrives as an embedded resource.
pub enum AssetPart {
    Text(String),
    Image { data: String, mime_type: String },
    Audio { data: String, mime_type: String },
    Resource { uri: String, mime_type: String, blob: String },
}

fn utf8_text(data: &[u8]) -> Option<String> {
    if data.contains(&0) {
        return None;
    }
    std::str::from_utf8(data).ok().map(str::to_string)
}

fn asset_result(meta: &AssetMeta, data: &[u8]) -> Vec<AssetPart> {
    let ext = extname(&meta.name).to_lowercase();
    let base64 = || base64::engine::general_purpose::STANDARD.encode(data);
    let image = match ext.as_str() {
        ".png" => Some("image/png"),
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".gif" => Some("image/gif"),
        ".webp" => Some("image/webp"),
        _ => None,
    };
    let audio = match ext.as_str() {
        ".mp3" => Some("audio/mpeg"),
        ".wav" => Some("audio/wav"),
        ".m4a" => Some("audio/mp4"),
        ".ogg" => Some("audio/ogg"),
        _ => None,
    };
    let binary = match ext.as_str() {
        ".pdf" => Some("application/pdf"),
        ".mp4" => Some("video/mp4"),
        ".mov" => Some("video/quicktime"),
        ".webm" => Some("video/webm"),
        _ => None,
    };
    let text = if binary.is_some() { None } else { utf8_text(data) };
    let (mime_type, content) = if let Some(mime) = image {
        (mime.to_string(), AssetPart::Image { data: base64(), mime_type: mime.into() })
    } else if let Some(mime) = audio {
        (mime.to_string(), AssetPart::Audio { data: base64(), mime_type: mime.into() })
    } else if let Some(text) = text {
        ("text/plain".to_string(), AssetPart::Text(text))
    } else {
        let mime = binary.unwrap_or("application/octet-stream").to_string();
        let uri = format!("{}://asset/{}", crate::deep_links::DEEP_LINK_SCHEME, path_escape(&meta.path));
        (mime.clone(), AssetPart::Resource { uri, mime_type: mime, blob: base64() })
    };
    let facts = json!({ "path": meta.path, "name": meta.name, "size": meta.size, "mimeType": mime_type });
    vec![AssetPart::Text(serde_json::to_string_pretty(&facts).unwrap_or_default()), content]
}

/// Go's url.PathEscape, so a resource's URI reads the same from either
/// engine: encodeURIComponent, then `!'()*` escaped and `$&+=:@` kept.
fn path_escape(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~$&+=:@".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `(size / MIB).toFixed(1)`: half rounds up, on the exact value.
fn mib_one_decimal(size: u64) -> String {
    let tenths = (size as u128 * 20 + MIB as u128) / (2 * MIB as u128);
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn too_large_to_read(name: &str, size: u64) -> String {
    format!("{name} is {} MiB; read_asset returns files up to {} MiB.", mib_one_decimal(size), MAX_READ_ASSET_BYTES / MIB)
}

/* ---------- Tools --------------------------------------------------------------- */

/// What an asset tool answers: JSON like any other tool, or read_asset's
/// several content parts.
pub enum AssetAnswer {
    Json(Value),
    Parts(Vec<AssetPart>),
}

fn to_json<T: Serialize>(value: T) -> Result<AssetAnswer, String> {
    serde_json::to_value(value).map(AssetAnswer::Json).map_err(|e| e.to_string())
}

pub fn list_assets_tool(args: &Args, backend: &dyn Backend) -> Result<Value, String> {
    if optional_boolean(args, "deleted")?.unwrap_or(false) {
        return serde_json::to_value(backend.list_deleted_assets()?).map_err(|e| e.to_string());
    }
    let listed: Vec<AssetMeta> = backend.list_assets()?.into_iter().filter(|a| !is_loose_csv_internal(&a.path)).collect();
    serde_json::to_value(listed).map_err(|e| e.to_string())
}

/// The asset tools other than list_assets, or None for another tool.
pub fn call_asset_tool(name: &str, args: &Args, backend: &dyn Backend) -> Option<Result<AssetAnswer, String>> {
    let run = || -> Result<AssetAnswer, String> {
        match name {
            "read_asset" => {
                let meta = find_asset(backend, require_string(args, "path")?)?;
                if meta.size > MAX_READ_ASSET_BYTES {
                    return Err(too_large_to_read(&meta.name, meta.size));
                }
                let data = backend.read_asset(&meta.path)?;
                if data.len() as u64 > MAX_READ_ASSET_BYTES {
                    return Err(too_large_to_read(&meta.name, data.len() as u64));
                }
                Ok(AssetAnswer::Parts(asset_result(&meta, &data)))
            }
            "upload_asset" => {
                let filename = require_string(args, "filename")?;
                let encoded = require_string(args, "content_base64")?;
                check_asset_name(filename)?;
                // The name it will really have has to pass too.
                let cleaned = imported_asset_filename(js_trim(filename));
                check_asset_name(&cleaned)?;
                let bytes = decode_asset_base64(encoded)?;
                to_json(backend.import_asset(&cleaned, &bytes)?)
            }
            "rename_asset" => {
                let rel = require_string(args, "path")?;
                let new_name = require_string(args, "name")?;
                check_asset_new_name(new_name)?;
                let meta = find_asset(backend, rel)?;
                refuse_database_file(&meta, backend)?;
                to_json(backend.rename_asset(&meta.path, js_trim(new_name))?)
            }
            "move_asset" => {
                let rel = require_string(args, "path")?;
                let raw_dir = require_string(args, "targetDir")?;
                let meta = find_asset(backend, rel)?;
                refuse_database_file(&meta, backend)?;
                let dir = check_asset_target_dir(backend, raw_dir)?;
                to_json(backend.move_asset(&meta.path, &dir)?)
            }
            "duplicate_asset" => {
                let meta = find_asset(backend, require_string(args, "path")?)?;
                refuse_database_file(&meta, backend)?;
                to_json(backend.duplicate_asset(&meta.path)?)
            }
            "delete_asset" => {
                let meta = find_asset(backend, require_string(args, "path")?)?;
                refuse_database_file(&meta, backend)?;
                to_json(backend.delete_asset(&meta.path)?)
            }
            "restore_asset" => {
                let token = require_string(args, "undoToken")?;
                let entry = backend
                    .list_deleted_assets()?
                    .into_iter()
                    .find(|d| d.undo_token == token)
                    .ok_or_else(|| deleted_asset_not_found(token))?;
                to_json(backend.restore_deleted_asset(&entry)?)
            }
            _ => unreachable!(),
        }
    };
    matches!(
        name,
        "read_asset" | "upload_asset" | "rename_asset" | "move_asset" | "duplicate_asset" | "delete_asset" | "restore_asset"
    )
    .then(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::backend::LocalBackend;

    fn vault(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let abs = dir.path().join(rel);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        dir
    }

    #[test]
    fn listing_is_every_loose_file() {
        let dir = vault(&[
            ("assets/a.png", b"png"),
            ("inbox/note.md", b"# n"),
            ("inbox/d.excalidraw", b"{}"),
            ("inbox/Db.base/data.csv", b"id"),
            ("Projects/x.pdf", b"%PDF"),
            (".zennotes/vault.json", b"{}"),
            ("inbox/n.md.123.1700000000000.tmp", b""),
        ]);
        let mut paths: Vec<String> = list_assets(dir.path()).unwrap().into_iter().map(|a| a.path).collect();
        paths.sort();
        assert_eq!(paths, vec!["Projects/x.pdf", "assets/a.png"]);
    }

    #[test]
    fn names_are_checked_like_upstream() {
        assert_eq!(imported_asset_filename("a/b:c#d.png"), "b-c-d.png");
        assert_eq!(imported_asset_filename("con.txt"), "_con.txt");
        assert_eq!(imported_asset_filename("x.png. "), "x.png");
        assert_eq!(imported_asset_filename("..."), "file");
        assert_eq!(check_asset_name(".x.png"), Err("A file name cannot start with a dot.".into()));
        assert_eq!(check_asset_name("a.md."), Err("Notes are not attachments: use create_note or rename_note.".into()));
        assert_eq!(check_asset_name("t.csv.base.json"), Err("That kind of file cannot be managed as an attachment.".into()));
        assert!(check_asset_new_name("a#b.png").unwrap_err().starts_with("A file name cannot contain"));
        assert_eq!(check_asset_new_name("NUL.png"), Err("That name is reserved on Windows, so choose another.".into()));
    }

    #[test]
    fn base64_is_read_leniently() {
        assert_eq!(decode_asset_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_asset_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_asset_base64("data:image/png;base64,aG\nk=").unwrap(), b"hi");
        assert_eq!(decode_asset_base64("data:text/plain,hi").unwrap_err(), "content_base64 is a data: URL that is not base64.");
        assert_eq!(decode_asset_base64("a").unwrap_err(), "content_base64 is not valid base64.");
        assert_eq!(decode_asset_base64("====").unwrap_err(), "content_base64 is empty.");
        assert_eq!(mib_one_decimal(5 * MIB + MIB / 4), "5.3");
        assert_eq!(path_escape("assets/a b(1)$.png"), "assets%2Fa%20b%281%29$.png");
    }

    #[test]
    fn delete_lands_in_the_apps_trash_and_restores() {
        let dir = vault(&[("assets/a.png", b"png")]);
        let backend = LocalBackend::new(dir.path());
        let deleted = backend.delete_asset("assets/a.png").unwrap();
        assert!(!dir.path().join("assets/a.png").exists());
        // The app's own Trash view reads the same store.
        let app_listing = crate::vault::assets::list_deleted_assets(dir.path());
        assert_eq!(app_listing.len(), 1);
        assert_eq!(app_listing[0].undo_token, deleted.undo_token);
        assert_eq!(backend.list_deleted_assets().unwrap(), vec![deleted.clone()]);
        fs::write(dir.path().join("assets/a.png"), b"new").unwrap();
        let restored = backend.restore_deleted_asset(&deleted).unwrap();
        assert_eq!(restored.path, "assets/a 2.png");
        assert!(backend.list_deleted_assets().unwrap().is_empty());
    }

    #[test]
    fn rename_and_move_never_overwrite() {
        let dir = vault(&[("assets/a.png", b"a"), ("assets/b.png", b"b"), ("dest/a.png", b"other")]);
        let root = dir.path();
        assert_eq!(rename_asset(root, "assets/a.png", "b.png", None).unwrap_err(), "an asset named \"b.png\" already exists in this folder");
        let moved = move_asset(root, "assets/a.png", "dest", None).unwrap();
        assert_eq!(moved.path, "dest/a 2.png");
        let copy = duplicate_asset(root, "assets/b.png").unwrap();
        assert_eq!(copy.path, "assets/b copy.png");
        assert_eq!(folders_as_listed(root, "dest/new"), Some("dest/new".into()));
        assert_eq!(folders_as_listed(root, "dest/bad."), None);
    }
}
