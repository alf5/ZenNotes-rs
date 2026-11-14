//! Vault operations behind the MCP tools, ported from upstream
//! apps/desktop/src/mcp/vault-ops.ts. That file is upstream's own copy of the
//! vault logic for the MCP process (and the `zn` CLI), and differs in places
//! from the main process `crate::vault` mirrors: its own title sanitizing and
//! " 2" counters, frontmatter tags in `tags`, `.md`-only listings, and no
//! inbound wikilink rewrite on rename (the `rename_note` tool tells the model
//! to check backlinks first). The tools are held to it, so this follows it;
//! the sidecar bookkeeping (comments, creation dates, agent edit records) is
//! the port's shared code in `crate::vault::sidecars`. The attachment half
//! lives in assets.rs.
//!
//! As upstream's server does for every vault it opens
//! (`treatRecordPagesAsNotes`), a database's record pages are notes here: the
//! note walk goes into `.base` folders, the folder listing does not.
//!
//! Remapped system folders (vault.json `systemFolderPaths`, #398) are not
//! honoured, matching the rest of this port (see GAP-ANALYSIS.md).
//!
//! Every path an assistant passes is a vault-relative POSIX path and goes
//! through `resolve_safe` (lexical, then through every symbolic link on the
//! way) and, for a note or a folder, the listings' own classification
//! (`resolve_note`, `bucket_dir`) before anything is read or written.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use fancy_regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use super::links::open_note_link;
use super::scope::{is_js_ws, js_trim};
use crate::ipc::types::NoteComment;
use crate::vault::atomic::{atomic_write_target, rename_with_retry, write_file_atomic};
use crate::vault::comments;
use crate::vault::layout::{ATTACHMENTS_DIRS, FOLDERS, INTERNAL_VAULT_DIR, VAULT_SETTINGS_FILE};
use crate::vault::metadata::strip_code_content;
use crate::vault::notes::{created_at_ms, to_posix};
use crate::vault::settings::get_vault_settings;
use crate::vault::sidecars::{
    agent_edit_folder_path, file_identity, note_metadata_path, prepare_note_creation, read_note_created_at, rel_of,
    relocate_folder_trees, relocate_note, remove_agent_edit_record, remove_note_creation,
};

pub const LIVE_FOLDERS: [&str; 3] = ["inbox", "quick", "archive"];
const DEFAULT_TYPST_PREAMBLE_FOLDER: &str = "typst";
const OBSIDIAN_EXCALIDRAW_SUFFIX: &str = ".excalidraw.md";
/// How many taken names the search for a free one goes through.
const MAX_UNIQUE_ATTEMPTS: usize = 10000;
/// Leaves room in a file name for a copy number and an extension.
const MAX_TITLE_BYTES: usize = 200;

/// A note as the tools describe it. Field order is upstream's, so the JSON a
/// model reads is the same either way.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMeta {
    pub path: String,
    pub link: String,
    pub title: String,
    pub folder: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub size: u64,
    pub tags: Vec<String>,
    pub wikilinks: Vec<String>,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteContent {
    #[serde(flatten)]
    pub meta: NoteMeta,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderRef {
    pub folder: String,
    pub subpath: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetMeta {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub updated_at: i64,
    /// After a rename or move: the notes it could not read or write, whose
    /// links may still use the old name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links_not_updated: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    pub path: String,
    pub link: String,
    pub title: String,
    pub folder: String,
    pub line_number: usize,
    pub line_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplaceResult {
    pub meta: NoteMeta,
    pub replacements: usize,
}

pub fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

/// JavaScript's `\s` (ECMAScript WhiteSpace + LineTerminator), which is not
/// Unicode White_Space: it has U+FEFF and lacks U+0085.
pub const JS_WS: &str = r"[\t\n\x{0B}\x{0C}\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

static FRONTMATTER_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^---\r?\n([\s\S]*?)\r?\n---\r?\n?"));
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!("{JS_WS}+")));

/* ---------- Errors -------------------------------------------------------- */

/// Node's wording for a failed file system call (`ENOENT: no such file or
/// directory, open '<path>'`), which is what upstream's tools say: an
/// assistant that learned to read "ENOENT" as "no such note" keeps working.
pub fn node_error(error: &std::io::Error, syscall: &str, abs: &Path) -> String {
    let p = abs.display();
    let (code, text) = match error.raw_os_error() {
        Some(1) => ("EPERM", "operation not permitted"),
        Some(2) => ("ENOENT", "no such file or directory"),
        Some(13) => ("EACCES", "permission denied"),
        Some(17) => ("EEXIST", "file already exists"),
        Some(20) => ("ENOTDIR", "not a directory"),
        Some(21) if syscall == "open" || syscall == "read" => {
            return "EISDIR: illegal operation on a directory, read".into();
        }
        Some(21) => ("EISDIR", "illegal operation on a directory"),
        Some(39) => ("ENOTEMPTY", "directory not empty"),
        _ if error.kind() == std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        _ => return format!("{error}"),
    };
    format!("{code}: {text}, {syscall} '{p}'")
}

pub fn read_error(error: &std::io::Error, abs: &Path) -> String {
    node_error(error, "open", abs)
}

fn not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn not_a_directory(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(20) || error.kind() == std::io::ErrorKind::NotADirectory
}

/// The text Node's `readFile(…, 'utf8')` hands over: invalid UTF-8 becomes
/// U+FFFD rather than an error.
pub fn read_text(abs: &Path) -> Result<String, String> {
    fs::read(abs).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).map_err(|e| read_error(&e, abs))
}

fn write_text(root: &Path, abs: &Path, text: &str) -> Result<(), String> {
    prepare_note_creation(root, &rel_of(root, abs))?;
    write_file_atomic(abs, text)
}

/// `fs.writeFile(abs, text, { flag: 'wx' })`: a new file only, never one that
/// appeared since the name was checked.
fn write_new_file(abs: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file =
        fs::OpenOptions::new().write(true).create_new(true).open(abs).map_err(|e| node_error(&e, "open", abs))?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

/* ---------- Paths --------------------------------------------------------- */

/// `path.resolve` on a vault root: absolute, lexically normalized.
pub fn abs_root(root: &Path) -> PathBuf {
    PathBuf::from(crate::vault::config::resolve_path(&root.to_string_lossy()))
}

/// Resolve a vault-relative path under `root`, refusing anything that lands
/// outside it: through `..`, or through a symbolic link on the way whose
/// target is outside. Components that do not exist yet are left alone (they
/// cannot be links until they are created).
pub fn resolve_safe(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let root_abs = abs_root(root);
    let abs = crate::vault::notes::resolve_safe(&root_abs, rel)?;
    if abs == root_abs {
        return Ok(abs);
    }
    let Ok(root_real) = fs::canonicalize(&root_abs) else { return Ok(abs) };
    let Ok(rest) = abs.strip_prefix(&root_abs) else { return Ok(abs) };
    let mut walk = root_abs.clone();
    for part in rest.components() {
        walk.push(part);
        let info = match fs::symlink_metadata(&walk) {
            Ok(info) => info,
            Err(e) if not_found(&e) || not_a_directory(&e) => return Ok(abs),
            Err(e) => return Err(node_error(&e, "lstat", &walk)),
        };
        if !info.file_type().is_symlink() {
            continue;
        }
        let target = fs::canonicalize(&walk).map_err(|e| node_error(&e, "realpath", &walk))?;
        if !target.starts_with(&root_real) {
            return Err(format!("Path escapes vault: {rel}"));
        }
        walk = target;
    }
    Ok(abs)
}

/// The vault-relative POSIX path of `abs`.
pub fn rel_path(root: &Path, abs: &Path) -> String {
    rel_of(&abs_root(root), abs)
}

/// Node's `path.extname`: from the last dot of the last segment, unless that
/// dot starts the name.
pub fn extname(p: &str) -> &str {
    let base = p.rsplit(['/', std::path::MAIN_SEPARATOR]).next().unwrap_or(p);
    if base == ".." {
        return "";
    }
    match base.rfind('.') {
        Some(i) if i > 0 => &base[i..],
        _ => "",
    }
}

/// Node's `path.basename`.
pub fn basename(p: &str) -> &str {
    let trimmed = p.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `path.basename(p, ext)`.
pub fn stem_of(name: &str) -> &str {
    let ext = extname(name);
    &name[..name.len() - ext.len()]
}

/* ---------- Layout -------------------------------------------------------- */

pub fn primary_notes_location(root: &Path) -> String {
    get_vault_settings(root).primary_notes_location
}

/// The directory that holds a top-level folder's notes: the vault root for
/// `inbox` in a root-mode vault.
pub fn folder_root(root: &Path, folder: &str) -> PathBuf {
    let root = abs_root(root);
    if folder == "inbox" && primary_notes_location(&root) == "root" {
        root
    } else {
        root.join(folder)
    }
}

/// Directory names at the vault root that are never inbox content.
pub fn hidden_root_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = ATTACHMENTS_DIRS.to_vec();
    names.extend([INTERNAL_VAULT_DIR, "quick", "archive", "trash"]);
    names
}

/// The top-level folder a vault path belongs to. A root-level file counts as
/// inbox (root-mode vaults keep their notes there); hidden names
/// (`.zennotes`, attachments) are not notes at all.
pub fn folder_of(root: &Path, abs: &Path) -> Option<String> {
    let rel = rel_path(root, abs);
    if rel.is_empty() || rel.starts_with("..") || Path::new(&rel).is_absolute() {
        return None;
    }
    let top = rel.split('/').next().unwrap_or("");
    let lower = top.to_lowercase();
    if let Some(folder) = FOLDERS.iter().find(|f| **f == lower) {
        return Some(folder.to_string());
    }
    if top.is_empty() || top.starts_with('.') || hidden_root_names().contains(&top) {
        return None;
    }
    Some("inbox".into())
}

/// The identity `fs.lstat(p, { bigint: true })` compares: the entry itself,
/// not where a link leads.
fn lstat_identity(p: &Path) -> Option<(u64, u64)> {
    let md = fs::symlink_metadata(p).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((md.dev(), md.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        let canonical = fs::canonicalize(p).ok()?;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        canonical.to_string_lossy().to_lowercase().hash(&mut h);
        Some((0, h.finish()))
    }
}

/// Whether a top-level name is one the root walk hides: the internal dir, an
/// attachments dir or another bucket's directory. A different spelling still
/// is one when it names the same directory on disk, as a case variant does on
/// macOS and Windows.
fn is_hidden_top_name(root: &Path, name: &str) -> bool {
    let hidden = hidden_root_names();
    if hidden.contains(&name) {
        return true;
    }
    let lower = name.to_lowercase();
    let variants: Vec<&str> = hidden.iter().copied().filter(|h| h.to_lowercase() == lower).collect();
    if variants.is_empty() {
        return false;
    }
    let root = abs_root(root);
    let Some(me) = lstat_identity(&root.join(name)) else { return false };
    variants.iter().any(|v| lstat_identity(&root.join(v)) == Some(me))
}

/// The bucket a note (`is_dir` false) or folder (`is_dir` true) at `abs`
/// belongs to as the listings see it, or None. They never descend into a dot
/// directory at any depth, nor show the internal dir, an attachments dir or
/// another bucket's directory as part of the inbox, so neither does this.
pub fn classify_path(root: &Path, abs: &Path, is_dir: bool) -> Option<String> {
    let rel = rel_path(root, abs);
    if rel.is_empty() || rel.starts_with("..") || Path::new(&rel).is_absolute() {
        return None;
    }
    let segments: Vec<&str> = rel.split('/').collect();
    let dirs = if is_dir { &segments[..] } else { &segments[..segments.len() - 1] };
    if dirs.iter().any(|s| s.starts_with('.')) {
        return None;
    }
    let folder = folder_of(root, abs)?;
    if folder != "inbox" {
        return Some(folder);
    }
    (!is_hidden_top_name(root, segments[0])).then_some(folder)
}

/// Resolve a note a tool reads, edits, moves or deletes, and refuse anything
/// outside the note folders BEFORE it is touched, so a tool can neither plant
/// comments, creation dates or edit records under `.zennotes/` nor carry
/// `.zennotes/vault.json` into a note folder.
pub fn resolve_note(root: &Path, rel: &str) -> Result<(PathBuf, String), String> {
    let abs = resolve_safe(root, rel)?;
    let folder = classify_path(root, &abs, false).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    Ok((abs, folder))
}

/// `subpath.replace(/^\/+|\/+$/g, '')`.
pub fn trim_slashes(s: &str) -> &str {
    s.trim_matches('/')
}

/// The directory a create, move, rename or delete names by bucket and
/// subpath. It must be a folder the listings show in that bucket: a subpath
/// through a dot directory, or one that reaches the internal dir, an
/// attachments dir or another bucket is refused. A subpath that resolves to
/// nothing, `x/..` included, is the bucket root itself, which callers that
/// need a subfolder refuse.
pub fn bucket_dir(root: &Path, folder: &str, subpath: &str) -> Result<PathBuf, String> {
    let base = folder_root(root, folder);
    let clean = trim_slashes(subpath);
    if clean.is_empty() {
        return Ok(base);
    }
    let abs = resolve_safe(&base, clean)?;
    if abs == base {
        return Ok(base);
    }
    if classify_path(root, &abs, true).as_deref() != Some(folder) {
        return Err(format!("Not a note folder: {clean}"));
    }
    Ok(abs)
}

/// Whether a vault-relative directory is, or sits under, a link. Anything but
/// plain absence counts as one, and so does a file with more of the path
/// below it, since it cannot be walked.
pub fn goes_through_link(root: &Path, rel: &str) -> bool {
    let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let mut walked = abs_root(root);
    for (at, seg) in segments.iter().enumerate() {
        walked.push(seg);
        match fs::symlink_metadata(&walked) {
            Ok(info) => {
                if info.file_type().is_symlink() {
                    return true;
                }
                if at < segments.len() - 1 && !info.is_dir() {
                    return true;
                }
            }
            Err(e) => return !not_found(&e),
        }
    }
    false
}

fn is_form_dir_name(name: &str) -> bool {
    name.to_lowercase().ends_with(".base")
}

fn is_md(name: &str) -> bool {
    name.to_lowercase().ends_with(".md")
}

/// A directory's entries as (path, name, is_dir, is_file), sorted by name.
/// Like Node's `readdir({ withFileTypes })`, a symlink is neither, so
/// symlinked notes and folders are not walked.
fn entries(dir: &Path) -> Vec<(PathBuf, String, bool, bool)> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<_> = read
        .flatten()
        .filter_map(|e| {
            let ft = e.file_type().ok()?;
            Some((e.path(), e.file_name().to_string_lossy().into_owned(), ft.is_dir(), ft.is_file()))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/* ---------- Vault settings ------------------------------------------------ */

pub fn json_settings(root: &Path) -> Option<Value> {
    let raw = fs::read_to_string(abs_root(root).join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// `.zennotes/vault.json` as stored; `{}` when missing, unreadable or not an
/// object (`readVaultSettingsRaw`).
pub fn vault_settings_raw(root: &Path) -> Value {
    match json_settings(root) {
        Some(v @ Value::Object(_)) => v,
        _ => Value::Object(Map::new()),
    }
}

/// shared-domain's `normalizeTemplateFolder`.
pub fn normalize_template_folder(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    let posix = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for seg in posix.split('/') {
        let s = js_trim(seg);
        if s.is_empty() {
            continue;
        }
        if s.starts_with('.') {
            return None;
        }
        parts.push(s);
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("/");
    (joined.encode_utf16().count() <= 512).then_some(joined)
}

/// `resolveTemplateFolder`: the template folder (#1024) vault.json names.
pub fn resolve_template_folder(settings: &Value) -> Option<String> {
    let templates = settings.get("templates")?;
    if !(templates.is_object() || templates.is_array()) {
        return None;
    }
    normalize_template_folder(templates.get("folder").unwrap_or(&Value::Null))
}

/// The vault's template folder, read straight off vault.json, or None.
pub fn read_template_folder(root: &Path) -> Option<String> {
    resolve_template_folder(&json_settings(root)?)
}

/// `isPathInTemplateFolder`: a whole-segment prefix, case-sensitive.
pub use crate::vault::settings::is_path_in_template_folder;

/// `outsideTemplateFolder`: the notes the tag tools count.
pub fn outside_template_folder(notes: Vec<NoteMeta>, folder: Option<&str>) -> Vec<NoteMeta> {
    notes.into_iter().filter(|n| !is_path_in_template_folder(&n.path, folder)).collect()
}

/* ---------- Markdown parsing ----------------------------------------------- */

fn unquote(v: &str) -> String {
    let trimmed = js_trim(v);
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() >= 2 {
        let (first, last) = (chars[0], chars[chars.len() - 1]);
        if (first == '"' || first == '\'') && first == last {
            return chars[1..chars.len() - 1].iter().collect();
        }
    }
    trimmed.to_string()
}

static FM_ITEM_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{JS_WS}*-{JS_WS}+(.*)$")));
static FM_KV_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^([A-Za-z0-9_][\w-]*){JS_WS}*:{JS_WS}*(.*)$")));

/// The `tags` key of a frontmatter block, read with vault-ops'
/// `parseTaskFrontmatter` rules: a scalar, an inline `[a, b]` list, or an
/// indented `- item` block list.
fn frontmatter_tag_values(block: &str) -> Vec<String> {
    let mut tags: Option<Vec<String>> = None;
    let mut list_key: Option<String> = None;
    for raw in block.split('\n') {
        if js_trim(raw).is_empty() || js_trim(raw).starts_with('#') {
            continue;
        }
        if let (Some(key), Ok(Some(item))) = (&list_key, FM_ITEM_RE.captures(raw)) {
            if raw.starts_with(is_js_ws) {
                if key == "tags" {
                    tags.get_or_insert_with(Vec::new).push(unquote(&item[1]));
                }
                continue;
            }
        }
        let Ok(Some(kv)) = FM_KV_RE.captures(raw) else {
            list_key = None;
            continue;
        };
        let key = kv[1].to_lowercase();
        let rest = js_trim(&kv[2]).to_string();
        if rest.is_empty() {
            if key == "tags" {
                tags = Some(Vec::new());
            }
            list_key = Some(key);
            continue;
        }
        list_key = None;
        if key != "tags" {
            continue;
        }
        tags = Some(if rest.starts_with('[') && rest.ends_with(']') {
            rest[1..rest.len() - 1].split(',').map(unquote).filter(|s| !s.is_empty()).collect()
        } else {
            vec![unquote(&rest)]
        });
    }
    tags.unwrap_or_default()
}

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"(?:^|{JS_WS})#(\p{{L}}[\p{{L}}0-9_/-]*)")));
static TAG_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"(?:,|{JS_WS})+")));

fn push_unique(seen: &mut Vec<String>, value: String) {
    if !seen.contains(&value) {
        seen.push(value);
    }
}

/// Frontmatter `tags` plus inline `#tags`. A bare scalar splits on commas and
/// whitespace, since `tags: daily, work` is two tags (#444).
fn extract_tags(body: &str) -> Vec<String> {
    let mut seen = Vec::new();
    if let Ok(Some(fm)) = FRONTMATTER_RE.captures(body) {
        for raw in frontmatter_tag_values(fm.get(1).map(|m| m.as_str()).unwrap_or("")) {
            for part in TAG_SPLIT_RE.split(js_trim(&raw)).flatten() {
                let normalized = js_trim(part.strip_prefix('#').unwrap_or(part));
                if !normalized.is_empty() {
                    push_unique(&mut seen, normalized.to_string());
                }
            }
        }
    }
    let markdown_body = FRONTMATTER_RE.replace(body, "");
    let stripped = strip_code_content(&markdown_body);
    for caps in TAG_RE.captures_iter(&stripped).flatten() {
        push_unique(&mut seen, caps[1].to_string());
    }
    seen
}

/// A table escapes the separator as `\|`; the backslash is not the target's.
static WIKILINK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\[([^\]|]+?)(?:\\?\|[^\]]+)?\]\]"));

pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let stripped = strip_code_content(body);
    let mut seen = Vec::new();
    for caps in WIKILINK_RE.captures_iter(&stripped).flatten() {
        let target = js_trim(&caps[1]);
        if !target.is_empty() && target != "\\" {
            push_unique(&mut seen, target.to_string());
        }
    }
    seen
}

static EXCERPT_FRONT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^---\n[\s\S]*?\n---\n"));
static MD_IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"!\[[^\]]*\]\([^)]*\)"));
static MD_LINK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[([^\]]+)\]\([^)]*\)"));
static WIKI_LABEL_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]"));
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"(?m)^#{{1,6}}{JS_WS}+")));
static EMPHASIS_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[*_~>]+"));

/// The first 220 UTF-16 units of `text`, never splitting a character.
pub fn js_slice(text: &str, units: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for ch in text.chars() {
        count += ch.len_utf16();
        if count > units {
            break;
        }
        out.push(ch);
    }
    out
}

fn build_excerpt(body: &str) -> String {
    let without_front = EXCERPT_FRONT_RE.replace(body, "");
    let text = strip_code_content(&without_front);
    let text = MD_IMAGE_RE.replace_all(&text, " ");
    let text = MD_LINK_RE.replace_all(&text, "$1");
    let text = WIKI_LABEL_RE.replace_all(&text, |c: &fancy_regex::Captures| {
        c.get(2).or(c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_default()
    });
    let text = HEADING_RE.replace_all(&text, "");
    let text = EMPHASIS_RE.replace_all(&text, "");
    let text = WHITESPACE_RE.replace_all(&text, " ");
    js_slice(js_trim(&text), 220)
}

/* ---------- Note metadata --------------------------------------------------- */

/// The vault's Typst preamble folder (#562), straight off vault.json.
fn typst_preamble_folder(root: &Path) -> String {
    let configured = json_settings(root)
        .and_then(|v| v.get("typstPreambles")?.get("folder")?.as_str().map(str::to_string));
    let Some(raw) = configured else { return DEFAULT_TYPST_PREAMBLE_FOLDER.into() };
    let trimmed = js_trim(&raw);
    let invalid = trimmed.is_empty()
        || trimmed.chars().count() > 128
        || trimmed.starts_with('.')
        || trimmed.chars().any(|c| "\\/:*?\"<>|#^[]".contains(c));
    if invalid { DEFAULT_TYPST_PREAMBLE_FOLDER.into() } else { trimmed.to_string() }
}

/// Preamble notes hold Typst source, whose `#let` tokens are variables, not
/// tags; agents see the same (empty) tag list the app shows.
fn is_typst_preamble_path(rel: &str, folder: &str) -> bool {
    let name = folder.to_lowercase();
    let parts: Vec<&str> = rel.split('/').collect();
    !name.is_empty() && parts[..parts.len().saturating_sub(1)].iter().any(|p| js_trim(p).to_lowercase() == name)
}

pub fn ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn title_of(abs: &Path) -> String {
    let name = abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    stem_of(&name).to_string()
}

pub fn read_meta(root: &Path, abs: &Path, folder: &str) -> Result<NoteMeta, String> {
    let md = fs::metadata(abs).map_err(|e| node_error(&e, "stat", abs))?;
    let body = read_text(abs).unwrap_or_default();
    let rel = rel_path(root, abs);
    let updated_at = ms(md.modified());
    let is_preamble = is_typst_preamble_path(&rel, &typst_preamble_folder(root));
    Ok(NoteMeta {
        link: open_note_link(&rel),
        title: title_of(abs),
        folder: folder.to_string(),
        created_at: read_note_created_at(&abs_root(root), &rel, created_at_ms(&md, updated_at)),
        updated_at,
        size: md.len(),
        tags: if is_preamble { Vec::new() } else { extract_tags(&body) },
        wikilinks: extract_wikilinks(&body),
        excerpt: build_excerpt(&body),
        path: rel,
    })
}

/* ---------- Listing ----------------------------------------------------------- */

fn walk_notes(folder: &str, dir: &Path, top: &Path, is_primary_root: bool, out: &mut Vec<(PathBuf, String)>) {
    for (full, name, is_dir, is_file) in entries(dir) {
        if is_dir {
            // A database folder's record pages are notes to an assistant.
            if name.starts_with('.') {
                continue;
            }
            // In a root-mode vault the root walk is inbox; quick/, archive/,
            // trash/ and attachments are walked as their own folders.
            if is_primary_root && dir == top && hidden_root_names().contains(&name.as_str()) {
                continue;
            }
            walk_notes(folder, &full, top, is_primary_root, out);
        } else if is_file && is_md(&name) {
            out.push((full, folder.to_string()));
        }
    }
}

/// Every note file as (absolute path, top-level folder): `.md` files only,
/// never inside a dot-folder or a symlink. The one walk behind `list_notes`
/// and the task scan, so both see the same notes.
pub fn list_note_files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for folder in FOLDERS {
        let top = folder_root(root, folder);
        let is_primary_root = folder == "inbox" && top == abs_root(root);
        walk_notes(folder, &top, &top, is_primary_root, &mut out);
    }
    out
}

pub fn list_notes(root: &Path) -> Vec<NoteMeta> {
    list_note_files(root).into_iter().filter_map(|(abs, folder)| read_meta(root, &abs, &folder).ok()).collect()
}

fn walk_folders(
    folder: &str,
    dir: &Path,
    top: &Path,
    is_primary_root: bool,
    subpath: &str,
    databases: bool,
    out: &mut Vec<FolderRef>,
) {
    for (full, name, is_dir, _) in entries(dir) {
        if !is_dir || name.starts_with('.') {
            continue;
        }
        if is_primary_root && dir == top && hidden_root_names().contains(&name.as_str()) {
            continue;
        }
        let next = if subpath.is_empty() { name.clone() } else { format!("{subpath}/{name}") };
        if is_form_dir_name(&name) {
            if databases {
                out.push(FolderRef { folder: folder.to_string(), subpath: next });
            }
            continue;
        }
        if !databases {
            out.push(FolderRef { folder: folder.to_string(), subpath: next.clone() });
        }
        walk_folders(folder, &full, top, is_primary_root, &next, databases, out);
    }
}

fn walk_all_folders(root: &Path, databases: bool) -> Vec<FolderRef> {
    let mut out = Vec::new();
    for folder in FOLDERS {
        let top = folder_root(root, folder);
        let is_primary_root = folder == "inbox" && top == abs_root(root);
        walk_folders(folder, &top, &top, is_primary_root, "", databases, &mut out);
    }
    out
}

/// Every user folder, grouped by top-level folder; a database folder is not
/// one.
pub fn list_folders(root: &Path) -> Vec<FolderRef> {
    walk_all_folders(root, false)
}

/// Every `.base` database folder, the companion `list_folders` leaves out.
pub fn list_database_dirs(root: &Path) -> Vec<FolderRef> {
    walk_all_folders(root, true)
}

/* ---------- Read / write / create ----------------------------------------------- */

pub fn read_note(root: &Path, rel: &str) -> Result<NoteContent, String> {
    let (abs, folder) = resolve_note(root, rel)?;
    // A note is a Markdown file. Anything else in a note folder (a root-mode
    // vault that is also a code checkout holds plenty) is not one to read.
    if extname(&abs.to_string_lossy()).to_lowercase() != ".md" {
        return Err(format!("Not a Markdown note: {rel}"));
    }
    let body = read_text(&abs)?;
    Ok(NoteContent { meta: read_meta(root, &abs, &folder)?, body })
}

/// `fs.access(abs, W_OK)`: a read-only note is refused before the atomic
/// write, which asks only the folder.
fn check_writable(abs: &Path) -> Result<(), String> {
    match fs::metadata(abs) {
        Ok(md) if md.permissions().readonly() => {
            Err(format!("EACCES: permission denied, access '{}'", abs.display()))
        }
        _ => Ok(()),
    }
}

pub fn write_note(root: &Path, rel: &str, body: &str) -> Result<NoteMeta, String> {
    let (abs, folder) = resolve_note(root, rel)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| node_error(&e, "mkdir", parent))?;
    }
    prepare_note_creation(&abs_root(root), &rel_path(root, &abs))?;
    check_writable(&abs)?;
    write_file_atomic(&abs, body)?;
    read_meta(root, &abs, &folder)
}

/// `base`, or `base 2`, `base 3`, … — the first name free in `dir`. Not
/// `exists()`, which follows a link: a link to nothing would read as a free
/// name, and the write would land where it points.
fn unique_title(dir: &Path, base: &str) -> Result<String, String> {
    let mut candidate = base.to_string();
    for n in 2..=MAX_UNIQUE_ATTEMPTS {
        let abs = dir.join(format!("{candidate}.md"));
        match fs::symlink_metadata(&abs) {
            Ok(_) => candidate = format!("{base} {n}"),
            Err(e) if not_found(&e) => return Ok(candidate),
            Err(e) => return Err(node_error(&e, "lstat", &abs)),
        }
    }
    Err(format!("no free name for \"{base}\" after {MAX_UNIQUE_ATTEMPTS} tries"))
}

/// Whole characters only, so a cut never leaves half of one behind.
fn cut_to_bytes(text: &str, limit: usize) -> &str {
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        if i + ch.len_utf8() > limit {
            break;
        }
        end = i + ch.len_utf8();
    }
    &text[..end]
}

/// A note's copy: a title at the limit gives way to the word, so a copy of a
/// copy stays a name every other tool can write.
pub fn copy_title(title: &str) -> String {
    let word = " copy";
    format!("{}{word}", cut_to_bytes(title, MAX_TITLE_BYTES - word.len()).trim_end_matches(is_js_ws))
}

static UNSAFE_TITLE_RE: LazyLock<Regex> = LazyLock::new(|| re(r#"[\\/:\x00-\x1f*?"<>|]"#));

/// Filenames must be safe on all three OSes: path separators, control
/// characters and the reserved punctuation become `-`, and the title is cut
/// to 200 bytes of whole characters. (A lone surrogate, which upstream turns
/// into U+FFFD here, cannot reach a Rust string.)
pub fn sanitize_title(raw: &str) -> String {
    let replaced = UNSAFE_TITLE_RE.replace_all(raw, "-");
    let collapsed = WHITESPACE_RE.replace_all(&replaced, " ");
    let title = js_trim(cut_to_bytes(js_trim(&collapsed), MAX_TITLE_BYTES));
    if title.is_empty() { "Untitled".into() } else { title.to_string() }
}

pub fn create_note(
    root: &Path,
    folder: &str,
    title: Option<&str>,
    subpath: &str,
    body: Option<&str>,
) -> Result<NoteMeta, String> {
    if folder == "trash" {
        return Err("Refusing to create a note directly in trash/".into());
    }
    let base = sanitize_title(title.unwrap_or("Untitled"));
    let dir = bucket_dir(root, folder, subpath)?;
    fs::create_dir_all(&dir).map_err(|e| node_error(&e, "mkdir", &dir))?;
    let final_title = unique_title(&dir, &base)?;
    let abs = dir.join(format!("{final_title}.md"));
    let content = body.map(str::to_string).unwrap_or_else(|| format!("# {final_title}\n\n"));
    let rel = rel_path(root, &abs);
    let vault = abs_root(root);
    remove_note_creation(&vault, &rel, false)?;
    // A record left by an earlier note of this name would offer a bogus review.
    remove_agent_edit_record(&vault, &rel, false)?;
    write_new_file(&abs, content.as_bytes())?;
    read_meta(root, &abs, folder)
}

fn sync_title_heading_on_rename() -> bool {
    super::portable_pref("editor", "sync_title_heading_on_rename").unwrap_or(true)
}

static H1_LINE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^( {0,3})#(?:[ \t].*)?$"));
static FRONTMATTER_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^---\r?\n[\s\S]*?\r?\n---(?:\r?\n|$)"));

/// Port of shared-domain's `retitleLeadingHeading`: the first non-blank line
/// after any frontmatter, when it is an `# H1`, takes the new title.
fn retitle_leading_heading(body: &str, title: &str) -> String {
    if title.is_empty() || title.contains(['\r', '\n']) {
        return body.to_string();
    }
    let mut line_start = match FRONTMATTER_BLOCK_RE.find(body) {
        Ok(Some(m)) => m.end(),
        _ => 0,
    };
    while line_start < body.len() {
        let nl = body[line_start..].find('\n').map(|i| i + line_start);
        let line_end = nl.unwrap_or(body.len());
        let raw = &body[line_start..line_end];
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if !js_trim(line).is_empty() {
            let Ok(Some(m)) = H1_LINE_RE.captures(line) else { return body.to_string() };
            let next = format!("{}# {title}", &m[1]);
            if next == line {
                return body.to_string();
            }
            return format!("{}{next}{}", &body[..line_start], &body[line_start + line.len()..]);
        }
        match nl {
            Some(i) => line_start = i + 1,
            None => break,
        }
    }
    body.to_string()
}

fn is_obsidian_excalidraw_markdown(content: &str) -> bool {
    static FM: LazyLock<Regex> = LazyLock::new(|| re(r"^---\r?\n([\s\S]*?)\r?\n---"));
    static MARKER: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^excalidraw-plugin:\s*\S"));
    match FM.captures(content) {
        Ok(Some(c)) => MARKER.is_match(&c[1]).unwrap_or(false),
        _ => false,
    }
}

/// Rewrite the note's leading `# Heading` to its new filename when the user
/// has "Sync title heading on rename" on (#455), as the app's own renames do.
/// Never an Obsidian drawing, and a failure here never undoes the rename.
fn sync_title_heading(source: &Path, target: &Path, title: &str) {
    let is_drawing = |p: &Path| p.to_string_lossy().to_lowercase().ends_with(OBSIDIAN_EXCALIDRAW_SUFFIX);
    if is_drawing(source) || is_drawing(target) || !sync_title_heading_on_rename() {
        return;
    }
    let Ok(body) = fs::read_to_string(target) else { return };
    if is_obsidian_excalidraw_markdown(&body) {
        return;
    }
    let next = retitle_leading_heading(&body, title);
    if next != body {
        let _ = write_file_atomic(target, &next);
    }
}

pub fn rename_note(root: &Path, rel: &str, next_title: &str) -> Result<NoteMeta, String> {
    let (abs, folder) = resolve_note(root, rel)?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| abs_root(root));
    let trimmed = sanitize_title(next_title);
    let target = dir.join(format!("{trimmed}.md"));
    if target != abs {
        if target.exists() {
            let same = matches!((file_identity(&abs), file_identity(&target)), (Ok(a), Ok(b)) if a == b);
            if !same {
                return Err(format!("A note named \"{trimmed}\" already exists in {folder}"));
            }
        }
        // Comments, the creation date and the edit record travel with the note.
        relocate_note(&abs_root(root), &rel_path(root, &abs), &target, || Ok(()))?;
    }
    sync_title_heading(&abs, &target, &trimmed);
    read_meta(root, &target, &folder)
}

/// The note's directory relative to its top-level folder ('' at the folder
/// root): archive and trash moves carry the subfolder along so the reverse
/// move restores it.
fn folder_subpath_of(root: &Path, abs: &Path) -> String {
    let Some(folder) = folder_of(root, abs) else { return String::new() };
    let source_root = folder_root(root, &folder);
    let Some(dir) = abs.parent() else { return String::new() };
    match dir.strip_prefix(&source_root) {
        Ok(rel) => to_posix(&rel.to_string_lossy()),
        Err(_) => String::new(),
    }
}

fn move_between_folders(root: &Path, rel: &str, target: &str) -> Result<NoteMeta, String> {
    let (abs, _) = resolve_note(root, rel)?;
    let dest_dir = bucket_dir(root, target, &folder_subpath_of(root, &abs))?;
    fs::create_dir_all(&dest_dir).map_err(|e| node_error(&e, "mkdir", &dest_dir))?;
    let base = title_of(&abs);
    let dest = dest_dir.join(format!("{}.md", unique_title(&dest_dir, &base)?));
    relocate_note(&abs_root(root), &rel_path(root, &abs), &dest, || Ok(()))?;
    read_meta(root, &dest, target)
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

pub fn move_note(root: &Path, old_rel: &str, target_folder: &str, target_subpath: &str) -> Result<NoteMeta, String> {
    let (old_abs, folder) = resolve_note(root, old_rel)?;
    let dest_dir = bucket_dir(root, target_folder, target_subpath)?;
    if old_abs.parent() == Some(dest_dir.as_path()) {
        return read_meta(root, &old_abs, &folder);
    }
    fs::create_dir_all(&dest_dir).map_err(|e| node_error(&e, "mkdir", &dest_dir))?;
    let name = old_abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = extname(&name);
    let dest = dest_dir.join(format!("{}{ext}", unique_title(&dest_dir, stem_of(&name))?));
    relocate_note(&abs_root(root), &rel_path(root, &old_abs), &dest, || Ok(()))?;
    read_meta(root, &dest, target_folder)
}

pub fn duplicate_note(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    let (abs, folder) = resolve_note(root, rel)?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| abs_root(root));
    let name = abs.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = extname(&name);
    let dest = dir.join(format!("{}{ext}", unique_title(&dir, &copy_title(stem_of(&name)))?));
    let body = read_text(&abs)?;
    remove_note_creation(&abs_root(root), &rel_path(root, &dest), false)?;
    write_new_file(&dest, body.as_bytes())?;
    read_meta(root, &dest, &folder)
}

fn remove_path(path: &Path) {
    let _ = if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

pub fn delete_note(root: &Path, rel: &str) -> Result<(), String> {
    let (abs, _) = resolve_note(root, rel)?;
    let note_path = rel_path(root, &abs);
    let vault = abs_root(root);
    remove_path(&abs);
    // A note's discussion goes with it: left behind, it would be taken over
    // by the next note created under this name.
    let _ = fs::remove_file(comments::comments_path(&vault, &note_path));
    remove_note_creation(&vault, &note_path, false)?;
    remove_agent_edit_record(&vault, &note_path, false)
}

pub fn empty_trash(root: &Path) {
    let vault = abs_root(root);
    let trash = folder_root(root, "trash");
    let Ok(read) = fs::read_dir(&trash) else { return };
    for entry in read.flatten() {
        remove_path(&entry.path());
    }
    let trash_rel = rel_path(root, &trash);
    if let Ok(dir) = resolve_safe(&comments::comments_root(&vault), &trash_rel) {
        remove_path(&dir);
    }
    let _ = remove_note_creation(&vault, &trash_rel, true);
    let _ = remove_agent_edit_record(&vault, &trash_rel, true);
}

pub fn create_folder(root: &Path, top: &str, subpath: &str) -> Result<(), String> {
    let abs = bucket_dir(root, top, subpath)?;
    if abs == folder_root(root, top) {
        return Err("Folder name is required".into());
    }
    fs::create_dir_all(&abs).map_err(|e| node_error(&e, "mkdir", &abs))
}

/// shared-domain's `normalizeTasksExcludedFolders` (also the included list).
fn normalize_tasks_folder_list(value: Option<&Value>) -> Vec<String> {
    super::tasks::normalize_tasks_folders(value)
}

/// Repoint a `<bucket>:<subpath>` keyed map (the folder icons and colors) at
/// a renamed folder and everything inside it, or None when nothing moves; an
/// entry that moves wins over a stale one already at its new key.
fn rewrite_folder_keyed_map(map: &Map<String, Value>, folder: &str, old_sub: &str, new_sub: &str) -> Option<Map<String, Value>> {
    let exact = format!("{folder}:{old_sub}");
    let mut kept = Map::new();
    let mut moved = Map::new();
    for (key, value) in map {
        if *key == exact || key.starts_with(&format!("{exact}/")) {
            moved.insert(format!("{folder}:{new_sub}{}", &key[exact.len()..]), value.clone());
        } else {
            kept.insert(key.clone(), value.clone());
        }
    }
    if moved.is_empty() {
        return None;
    }
    // `{ ...kept, ...moved }`: a key already there keeps its place.
    for (key, value) in moved {
        kept.insert(key, value);
    }
    Some(kept)
}

/// Keep vault.json's path-keyed settings naming the same folders after a
/// rename or move, as desktop main, the server and zn do: the Tasks folder
/// lists, the template folder, and the folder icons and colors. Every other
/// key is written back as read, and vault.json is written only when an entry
/// moves (`rewriteSettingsForFolderRename`).
fn rewrite_settings_for_folder_rename(
    root: &Path,
    folder: &str,
    old_sub: &str,
    new_sub: &str,
    old_rel: &str,
    new_rel: &str,
) -> Result<(), String> {
    let settings_path = abs_root(root).join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE);
    let Ok(text) = fs::read_to_string(&settings_path) else { return Ok(()) };
    let Ok(Value::Object(mut raw)) = serde_json::from_str::<Value>(&text) else { return Ok(()) };
    let mut changed = false;
    if let Some(Value::Object(tasks)) = raw.get("tasks").cloned() {
        let mut lists = tasks.clone();
        for key in ["excludedFolders", "includedFolders"] {
            if tasks.contains_key(key) {
                let normalized = normalize_tasks_folder_list(tasks.get(key));
                lists.insert(key.into(), Value::Array(normalized.into_iter().map(Value::String).collect()));
            }
        }
        let inside = |key: &str| {
            lists.get(key).and_then(Value::as_array).is_some_and(|list| {
                list.iter().filter_map(Value::as_str).any(|e| e == old_rel || e.starts_with(&format!("{old_rel}/")))
            })
        };
        if inside("excludedFolders") || inside("includedFolders") {
            if !old_rel.is_empty() && !new_rel.is_empty() && old_rel != new_rel {
                for key in ["excludedFolders", "includedFolders"] {
                    let Some(list) = lists.get(key).and_then(Value::as_array).cloned() else { continue };
                    let moved: Vec<Value> = list
                        .iter()
                        .map(|entry| match entry.as_str() {
                            Some(e) if e == old_rel || e.starts_with(&format!("{old_rel}/")) => {
                                Value::String(format!("{new_rel}{}", &e[old_rel.len()..]))
                            }
                            _ => entry.clone(),
                        })
                        .collect();
                    let next = normalize_tasks_folder_list(Some(&Value::Array(moved)));
                    lists.insert(key.into(), Value::Array(next.into_iter().map(Value::String).collect()));
                }
            }
            raw.insert("tasks".into(), Value::Object(lists));
            changed = true;
        }
    }
    // The template folder (#1024) is an on-disk path too.
    let templates = raw.get("templates").filter(|t| t.is_object()).and_then(|t| {
        normalize_template_folder(t.get("folder").unwrap_or(&Value::Null))
    });
    if let Some(current) = templates {
        if !old_rel.is_empty()
            && !new_rel.is_empty()
            && old_rel != new_rel
            && (current == old_rel || current.starts_with(&format!("{old_rel}/")))
        {
            if let Some(moved) = normalize_template_folder(&Value::String(format!("{new_rel}{}", &current[old_rel.len()..]))) {
                raw.insert("templates".into(), serde_json::json!({ "folder": moved }));
                changed = true;
            }
        }
    }
    for key in ["folderIcons", "folderColors"] {
        let next = raw.get(key).and_then(Value::as_object).and_then(|m| rewrite_folder_keyed_map(m, folder, old_sub, new_sub));
        if let Some(next) = next {
            raw.insert(key.into(), Value::Object(next));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    // Through a temporary file, so the app never reads a half-written vault.json.
    let target = atomic_write_target(&settings_path);
    let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let tmp = PathBuf::from(format!("{}.{}.{stamp}.tmp", target.to_string_lossy(), std::process::id()));
    let text = serde_json::to_string_pretty(&Value::Object(raw)).map_err(|e| e.to_string())?;
    write_new_file(&tmp, text.as_bytes())?;
    if let Err(e) = rename_with_retry(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(node_error(&e, "rename", &tmp));
    }
    Ok(())
}

pub fn rename_folder(root: &Path, top: &str, old_subpath: &str, new_subpath: &str) -> Result<String, String> {
    let (old_clean, new_clean) = (trim_slashes(old_subpath), trim_slashes(new_subpath));
    if old_clean.is_empty() || new_clean.is_empty() {
        return Err("Both old and new folder paths are required".into());
    }
    // Both ends are note folders of this bucket and neither is its root.
    let base = folder_root(root, top);
    let old_abs = bucket_dir(root, top, old_clean)?;
    let new_abs = bucket_dir(root, top, new_clean)?;
    if old_abs == base || new_abs == base {
        return Err("Both old and new folder paths are required".into());
    }
    if new_abs == old_abs {
        return Ok(new_clean.to_string());
    }
    if new_abs.starts_with(&old_abs) {
        return Err("Cannot move a folder into itself".into());
    }
    // A missing folder is an error, as it is in the app and in zn.
    fs::metadata(&old_abs).map_err(|e| node_error(&e, "stat", &old_abs))?;
    let vault = abs_root(root);
    let (old_rel, new_rel) = (rel_path(root, &old_abs), rel_path(root, &new_abs));
    let rel_to_base = |abs: &Path| to_posix(&abs.strip_prefix(&base).unwrap_or(abs).to_string_lossy());
    let (old_sub, new_sub) = (rel_to_base(&old_abs), rel_to_base(&new_abs));
    let comments_root = comments::comments_root(&vault);
    // The folder's comments, creation dates, agent edit records and the
    // vault.json settings that name it move as one; a failure puts all of
    // them back.
    relocate_folder_trees(
        vec![
            (old_abs, new_abs),
            (resolve_safe(&comments_root, &old_rel)?, resolve_safe(&comments_root, &new_rel)?),
            (note_metadata_path(&vault, &old_rel, true)?, note_metadata_path(&vault, &new_rel, true)?),
            (agent_edit_folder_path(&vault, &old_rel)?, agent_edit_folder_path(&vault, &new_rel)?),
        ],
        || rewrite_settings_for_folder_rename(&vault, top, &old_sub, &new_sub, &old_rel, &new_rel),
    )?;
    Ok(new_clean.to_string())
}

pub fn delete_folder(root: &Path, top: &str, subpath: &str) -> Result<(), String> {
    // `x/..` resolves to the bucket root, which in root mode is the whole
    // vault: checked on the resolved directory, not the string.
    let abs = bucket_dir(root, top, subpath)?;
    if abs == folder_root(root, top) {
        return Err("Cannot delete the top-level folder".into());
    }
    let vault = abs_root(root);
    let rel = rel_path(root, &abs);
    remove_path(&abs);
    remove_path(&resolve_safe(&comments::comments_root(&vault), &rel)?);
    remove_note_creation(&vault, &rel, true)?;
    remove_agent_edit_record(&vault, &rel, true)
}

/* ---------- Text search ----------------------------------------------------------- */

struct Search<'a> {
    root: &'a Path,
    needle: String,
    limit: f64,
    template_folder: Option<String>,
    out: Vec<TextMatch>,
}

impl Search<'_> {
    fn full(&self) -> bool {
        self.out.len() as f64 >= self.limit
    }

    fn walk(&mut self, folder: &str, dir: &Path, top: &Path, is_primary_root: bool) {
        if self.full() {
            return;
        }
        for (full, name, is_dir, is_file) in entries(dir) {
            if self.full() {
                return;
            }
            if is_dir {
                if name.starts_with('.') || (is_primary_root && dir == top && hidden_root_names().contains(&name.as_str())) {
                    continue;
                }
                if is_path_in_template_folder(&rel_path(self.root, &full), self.template_folder.as_deref()) {
                    continue;
                }
                self.walk(folder, &full, top, is_primary_root);
                continue;
            }
            if !is_file || !is_md(&name) {
                continue;
            }
            let rel = rel_path(self.root, &full);
            if is_path_in_template_folder(&rel, self.template_folder.as_deref()) {
                continue;
            }
            let Ok(body) = read_text(&full) else { continue };
            let title = title_of(&full);
            for (i, line) in body.split('\n').enumerate() {
                if self.full() {
                    break;
                }
                if line.to_lowercase().contains(&self.needle) {
                    self.out.push(TextMatch {
                        link: open_note_link(&rel),
                        path: rel.clone(),
                        title: title.clone(),
                        folder: folder.to_string(),
                        line_number: i + 1,
                        line_text: js_slice(js_trim(&WHITESPACE_RE.replace_all(line, " ")), 220),
                    });
                }
            }
        }
    }
}

/// Case-insensitive substring search over live notes, line by line, leaving
/// out the vault's template folder (#1024).
pub fn search_text(root: &Path, query: &str, limit: f64) -> Vec<TextMatch> {
    let trimmed = js_trim(query);
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut search =
        Search { root, needle: trimmed.to_lowercase(), limit, template_folder: read_template_folder(root), out: Vec::new() };
    for folder in LIVE_FOLDERS {
        if search.full() {
            break;
        }
        let top = folder_root(root, folder);
        let is_primary_root = folder == "inbox" && top == abs_root(root);
        search.walk(folder, &top, &top, is_primary_root);
    }
    search.out
}

/* ---------- Convenience edits ------------------------------------------------------ */

fn trim_trailing_newlines(s: &str) -> &str {
    s.trim_end_matches('\n')
}

/// The note body with `text` added after a blank line (shared-domain's
/// note-body-insert.ts).
pub fn append_to_body(body: &str, text: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    let sep = if normalized.ends_with('\n') || normalized.is_empty() { "" } else { "\n" };
    let gap = if normalized.is_empty() { "" } else { "\n" };
    format!("{normalized}{sep}{gap}{}\n", trim_trailing_newlines(text))
}

/// The note body with `text` inserted at the top, below any frontmatter.
pub fn prepend_to_body(body: &str, text: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    let snippet = format!("{}\n\n", trim_trailing_newlines(text));
    match FRONTMATTER_RE.find(&normalized) {
        Ok(Some(fm)) => format!("{}{snippet}{}", fm.as_str(), &normalized[fm.end()..]),
        _ => format!("{snippet}{normalized}"),
    }
}

/// Insert `text` before zero-based `line_number`, clamped to the body.
fn insert_at_line_in_body(body: &str, line_number: f64, text: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.split('\n').collect();
    let at = line_number.floor().max(0.0).min(lines.len() as f64) as usize;
    lines.splice(at..at, text.split('\n'));
    lines.join("\n")
}

fn edit_note(root: &Path, rel: &str, edit: impl FnOnce(&str) -> String) -> Result<NoteMeta, String> {
    let (abs, folder) = resolve_note(root, rel)?;
    let body = read_text(&abs)?;
    write_text(&abs_root(root), &abs, &edit(&body))?;
    read_meta(root, &abs, &folder)
}

pub fn append_to_note(root: &Path, rel: &str, text: &str) -> Result<NoteMeta, String> {
    edit_note(root, rel, |body| append_to_body(body, text))
}

pub fn prepend_to_note(root: &Path, rel: &str, text: &str) -> Result<NoteMeta, String> {
    edit_note(root, rel, |body| prepend_to_body(body, text))
}

pub fn insert_at_line(root: &Path, rel: &str, line_number: f64, text: &str) -> Result<NoteMeta, String> {
    edit_note(root, rel, |body| insert_at_line_in_body(body, line_number, text))
}

/// Literal (non-regex) find-and-replace; nothing is written without a match.
pub fn replace_in_note(root: &Path, rel: &str, find: &str, replace: &str, all: bool) -> Result<ReplaceResult, String> {
    let (abs, folder) = resolve_note(root, rel)?;
    let body = read_text(&abs)?;
    let (next, replacements) = if all {
        (body.replace(find, replace), body.matches(find).count())
    } else {
        match body.find(find) {
            Some(i) => (format!("{}{replace}{}", &body[..i], &body[i + find.len()..]), 1),
            None => (body.clone(), 0),
        }
    };
    if replacements > 0 {
        write_text(&abs_root(root), &abs, &next)?;
    }
    Ok(ReplaceResult { meta: read_meta(root, &abs, &folder)?, replacements })
}

/* ---------- Comments and backlinks -------------------------------------------------- */

pub fn read_note_comments(root: &Path, rel: &str) -> Vec<NoteComment> {
    comments::read_note_comments(&abs_root(root), rel)
}

/// Replace a note's comments; an empty list removes the sidecar.
pub fn write_note_comments(root: &Path, rel: &str, list: &[Value]) -> Result<Vec<NoteComment>, String> {
    comments::write_note_comments(&abs_root(root), rel, &Value::Array(list.to_vec()))
}

/// Every note that wikilinks to the note at `rel`, matched on title, never
/// counting the note itself.
pub fn backlinks(root: &Path, rel: &str) -> Result<Vec<NoteMeta>, String> {
    let abs = resolve_safe(root, rel)?;
    let rel_path = rel_path(root, &abs);
    let file_name = rel_path.rsplit('/').next().unwrap_or(&rel_path);
    let target = match file_name.rfind('.') {
        Some(dot) if dot > 0 => &file_name[..dot],
        _ => file_name,
    }
    .to_lowercase();
    Ok(list_notes(root)
        .into_iter()
        .filter(|m| m.path != rel_path && m.wikilinks.iter().any(|w| w.to_lowercase() == target))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let abs = dir.path().join(rel);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        dir
    }

    #[test]
    fn body_edits_match_upstream() {
        assert_eq!(append_to_body("a", "b\n\n"), "a\n\nb\n");
        assert_eq!(append_to_body("a\n", "b"), "a\n\nb\n");
        assert_eq!(append_to_body("", "b"), "b\n");
        assert_eq!(prepend_to_body("---\nx: 1\n---\nbody", "top"), "---\nx: 1\n---\ntop\n\nbody");
        assert_eq!(prepend_to_body("body", "top\n"), "top\n\nbody");
        assert_eq!(insert_at_line_in_body("a\nb", 1.0, "x\ny"), "a\nx\ny\nb");
        assert_eq!(insert_at_line_in_body("a\nb", 99.0, "x"), "a\nb\nx");
        assert_eq!(insert_at_line_in_body("a\nb", -3.0, "x"), "x\na\nb");
    }

    #[test]
    fn titles_and_tags() {
        assert_eq!(sanitize_title("a/b: c?"), "a-b- c-");
        assert_eq!(sanitize_title("   "), "Untitled");
        let long = "é".repeat(150);
        assert_eq!(sanitize_title(&long).len(), 200);
        assert_eq!(copy_title(&"x".repeat(200)), format!("{} copy", "x".repeat(195)));
        assert_eq!(
            extract_tags("---\ntags: [daily, \"work\"]\n---\n#café and `#code` #x/y"),
            vec!["daily", "work", "café", "x/y"]
        );
        assert_eq!(extract_tags("---\ntags:\n  - a b\n  - c\n---\n"), vec!["a", "b", "c"]);
        assert_eq!(retitle_leading_heading("---\na: 1\n---\n\n# Old\nbody", "New"), "---\na: 1\n---\n\n# New\nbody");
        assert_eq!(retitle_leading_heading("intro\n# Old", "New"), "intro\n# Old");
    }

    #[test]
    fn escaped_table_pipes_are_links() {
        assert_eq!(extract_wikilinks("| [[Note\\|alias]] | [[\\|x]] |"), vec!["Note"]);
    }

    #[test]
    fn paths_are_classified_like_the_listings() {
        let dir = vault(&[
            (".zennotes/vault.json", "{\"primaryNotesLocation\":\"inbox\"}"),
            ("inbox/a.md", "# a"),
            ("inbox/.hidden/b.md", "# b"),
            ("inbox/code.ts", "x"),
            ("Projects.base/pages/Row.md", "# row"),
        ]);
        let root = dir.path();
        assert!(read_note(root, "inbox/a.md").is_ok());
        assert_eq!(read_note(root, "inbox/.hidden/b.md").unwrap_err(), "Note not in a known folder: inbox/.hidden/b.md");
        assert_eq!(read_note(root, ".zennotes/vault.json").unwrap_err(), "Note not in a known folder: .zennotes/vault.json");
        assert_eq!(read_note(root, "inbox/code.ts").unwrap_err(), "Not a Markdown note: inbox/code.ts");
        assert_eq!(bucket_dir(root, "inbox", "../quick").unwrap_err(), "Path escapes vault: ../quick");
        assert_eq!(bucket_dir(root, "inbox", "a/.hidden").unwrap_err(), "Not a note folder: a/.hidden");
        assert_eq!(delete_folder(root, "inbox", "x/..").unwrap_err(), "Cannot delete the top-level folder");
        assert_eq!(create_folder(root, "inbox", "/").unwrap_err(), "Folder name is required");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_vault_is_refused() {
        let outside = vault(&[("secret.md", "# secret")]);
        let dir = vault(&[("inbox/a.md", "# a")]);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("inbox/link")).unwrap();
        assert_eq!(read_note(dir.path(), "inbox/link/secret.md").unwrap_err(), "Path escapes vault: inbox/link/secret.md");
    }

    #[test]
    fn record_pages_are_notes_and_database_folders_are_not_folders() {
        let dir = vault(&[("inbox/Db.base/pages/Row.md", "# Row"), ("inbox/Db.base/data.csv", "id\n")]);
        let paths: Vec<String> = list_notes(dir.path()).into_iter().map(|n| n.path).collect();
        assert_eq!(paths, vec!["inbox/Db.base/pages/Row.md"]);
        assert!(list_folders(dir.path()).is_empty());
        assert_eq!(list_database_dirs(dir.path()), vec![FolderRef { folder: "inbox".into(), subpath: "Db.base".into() }]);
    }

    #[test]
    fn folder_renames_carry_their_settings() {
        let dir = vault(&[
            (
                ".zennotes/vault.json",
                r#"{"primaryNotesLocation":"inbox","tasks":{"excludedFolders":["inbox/Work/Old"]},"templates":{"folder":"inbox/Work/T"},"folderIcons":{"inbox:Work":"star","inbox:Other":"x"}}"#,
            ),
            ("inbox/Work/a.md", "# a"),
        ]);
        rename_folder(dir.path(), "inbox", "Work", "Office").unwrap();
        let raw = vault_settings_raw(dir.path());
        assert_eq!(raw["tasks"]["excludedFolders"], serde_json::json!(["inbox/Office/Old"]));
        assert_eq!(raw["templates"]["folder"], "inbox/Office/T");
        assert_eq!(
            serde_json::to_string(&raw["folderIcons"]).unwrap(),
            r#"{"inbox:Other":"x","inbox:Office":"star"}"#
        );
        assert!(rename_folder(dir.path(), "inbox", "Gone", "Here").unwrap_err().starts_with("ENOENT: no such file or directory, stat '"));
    }

    #[test]
    fn template_folder_stays_out_of_search() {
        let dir = vault(&[
            (".zennotes/vault.json", r#"{"templates":{"folder":"inbox/Templates"}}"#),
            ("inbox/Templates/T.md", "needle"),
            ("inbox/a.md", "needle"),
        ]);
        let found: Vec<String> = search_text(dir.path(), "NEEDLE", 80.0).into_iter().map(|m| m.path).collect();
        assert_eq!(found, vec!["inbox/a.md"]);
    }
}
