//! Vault operations behind the MCP tools, ported from upstream
//! apps/desktop/src/mcp/vault-ops.ts. That file is upstream's own copy of the
//! vault logic for the MCP process (and the `zn` CLI), and differs in places
//! from the main process `crate::vault` mirrors: its own title sanitizing and
//! " 2" counters, frontmatter tags in `tags`, `.md`-only listings, and no
//! inbound wikilink rewrite on rename (the `rename_note` tool tells the model
//! to check backlinks first). The tools are held to it, so this follows it;
//! the sidecar bookkeeping (comments, creation dates, agent edit records) is
//! the port's shared code in `crate::vault::sidecars`.
//!
//! Remapped system folders (vault.json `systemFolderPaths`, #398) are not
//! honoured, matching the rest of this port (see GAP-ANALYSIS.md).
//!
//! Every path an assistant passes is a vault-relative POSIX path and goes
//! through `resolve_safe` before anything is read or written.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use fancy_regex::Regex;
use serde::Serialize;
use serde_json::Value;

use super::links::open_note_link;
use crate::ipc::types::NoteComment;
use crate::vault::atomic::write_file_atomic;
use crate::vault::comments;
use crate::vault::layout::{ATTACHMENTS_DIRS, FOLDERS, INTERNAL_VAULT_DIR, VAULT_SETTINGS_FILE};
use crate::vault::notes::{created_at_ms, resolve_safe, to_posix};
use crate::vault::settings::get_vault_settings;
use crate::vault::sidecars::{
    agent_edit_folder_path, note_metadata_path, prepare_note_creation, read_note_created_at,
    rel_of, relocate_folder_trees, relocate_note, remove_agent_edit_record, remove_note_creation,
};

pub const LIVE_FOLDERS: [&str; 3] = ["inbox", "quick", "archive"];
const DEFAULT_TYPST_PREAMBLE_FOLDER: &str = "typst";
const OBSIDIAN_EXCALIDRAW_SUFFIX: &str = ".excalidraw.md";

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

#[derive(Debug, Clone, Serialize)]
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

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

static FRONTMATTER_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^---\r?\n([\s\S]*?)\r?\n---\r?\n?"));
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));

/* ---------- Errors -------------------------------------------------------- */

/// Node's wording for a failed read, which is what upstream's tools say: an
/// assistant that learned to read "ENOENT" as "no such note" keeps working.
pub fn read_error(error: &std::io::Error, abs: &Path) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => {
            format!("ENOENT: no such file or directory, open '{}'", abs.display())
        }
        std::io::ErrorKind::IsADirectory => "EISDIR: illegal operation on a directory, read".into(),
        _ => format!("{error}"),
    }
}

fn read_text(abs: &Path) -> Result<String, String> {
    fs::read(abs)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| read_error(&e, abs))
}

fn write_text(root: &Path, abs: &Path, text: &str) -> Result<(), String> {
    prepare_note_creation(root, &rel_of(root, abs))?;
    write_file_atomic(abs, text)
}

/* ---------- Layout -------------------------------------------------------- */

pub fn primary_notes_location(root: &Path) -> String {
    get_vault_settings(root).primary_notes_location
}

/// The directory that holds a top-level folder's notes: the vault root for
/// `inbox` in a root-mode vault.
pub fn folder_root(root: &Path, folder: &str) -> PathBuf {
    if folder == "inbox" && primary_notes_location(root) == "root" {
        root.to_path_buf()
    } else {
        root.join(folder)
    }
}

/// Directory names at the vault root that are never inbox content.
fn hidden_root_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = ATTACHMENTS_DIRS.to_vec();
    names.extend([INTERNAL_VAULT_DIR, "quick", "archive", "trash"]);
    names
}

/// The top-level folder a vault path belongs to. A root-level file counts as
/// inbox (root-mode vaults keep their notes there); hidden names
/// (`.zennotes`, attachments) are not notes at all.
pub fn folder_of(root: &Path, abs: &Path) -> Option<String> {
    let rel = rel_of(root, abs);
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

/// Resolve a note an edit is about to write, refusing anything outside the
/// note folders BEFORE a byte lands, so a tool cannot plant comments,
/// creation dates or edit records under `.zennotes/`.
fn note_for_write(root: &Path, rel: &str) -> Result<(PathBuf, String), String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    Ok((abs, folder))
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

fn trim_slashes(s: &str) -> &str {
    s.trim_matches('/')
}

/* ---------- Markdown parsing ----------------------------------------------- */

static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^[ \t]*(`{3,}|~{3,})(.*)$"));
static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"`[^`\n]*`"));

/// Blank out fenced and inline code so the tag, link and excerpt scanners
/// never read code as content. Line-based and indentation-tolerant: a fence
/// nested under a list item is still a code block (#293).
fn strip_code_content(body: &str) -> String {
    if !body.contains('`') && !body.contains('~') {
        return body.to_string();
    }
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    let mut in_fence = false;
    let mut fence_char = ' ';
    let mut fence_len = 0;
    for line in lines.iter_mut() {
        if let Ok(Some(m)) = FENCE_RE.captures(line) {
            let marker = m.get(1).map(|x| x.as_str()).unwrap_or("");
            let rest = m.get(2).map(|x| x.as_str()).unwrap_or("");
            let ch = marker.chars().next().unwrap_or(' ');
            if !in_fence {
                // A backtick fence's info string may not contain a backtick (CommonMark).
                if ch == '~' || !rest.contains('`') {
                    in_fence = true;
                    fence_char = ch;
                    fence_len = marker.len();
                    *line = " ".into();
                    continue;
                }
            } else if ch == fence_char && marker.len() >= fence_len && rest.trim().is_empty() {
                in_fence = false;
                *line = " ".into();
                continue;
            }
        }
        if in_fence {
            *line = " ".into();
        }
    }
    INLINE_CODE_RE.replace_all(&lines.join("\n"), " ").into_owned()
}

fn unquote(v: &str) -> String {
    let trimmed = v.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() >= 2 {
        let (first, last) = (chars[0], chars[chars.len() - 1]);
        if (first == '"' || first == '\'') && first == last {
            return chars[1..chars.len() - 1].iter().collect();
        }
    }
    trimmed.to_string()
}

static FM_ITEM_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*-\s+(.*)$"));
static FM_KV_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Za-z0-9_][\w-]*)\s*:\s*(.*)$"));

/// The `tags` key of a frontmatter block, read with vault-ops'
/// `parseTaskFrontmatter` rules: a scalar, an inline `[a, b]` list, or an
/// indented `- item` block list.
fn frontmatter_tag_values(block: &str) -> Vec<String> {
    let mut tags: Option<Vec<String>> = None;
    let mut list_key: Option<String> = None;
    for raw in block.split('\n') {
        if raw.trim().is_empty() || raw.trim().starts_with('#') {
            continue;
        }
        if let (Some(key), Ok(Some(item))) = (&list_key, FM_ITEM_RE.captures(raw)) {
            if raw.starts_with(char::is_whitespace) {
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
        let rest = kv[2].trim().to_string();
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

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|\s)#(\p{L}[\p{L}\d_/-]*)"));
static TAG_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[,\s]+"));

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
            for part in TAG_SPLIT_RE.split(raw.trim()).flatten() {
                let normalized = part.strip_prefix('#').unwrap_or(part).trim();
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

static WIKILINK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\[([^\]|]+?)(?:\|[^\]]+)?\]\]"));

fn extract_wikilinks(body: &str) -> Vec<String> {
    let stripped = strip_code_content(body);
    let mut seen = Vec::new();
    for caps in WIKILINK_RE.captures_iter(&stripped).flatten() {
        push_unique(&mut seen, caps[1].trim().to_string());
    }
    seen
}

static EXCERPT_FRONT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^---\n[\s\S]*?\n---\n"));
static MD_IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"!\[[^\]]*\]\([^)]*\)"));
static MD_LINK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[([^\]]+)\]\([^)]*\)"));
static WIKI_LABEL_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]"));
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^#{1,6}\s+"));
static EMPHASIS_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[*_~>]+"));

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
    text.trim().chars().take(220).collect()
}

/* ---------- Note metadata --------------------------------------------------- */

fn json_settings(root: &Path) -> Option<Value> {
    let raw = fs::read_to_string(root.join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The vault's Typst preamble folder (#562), straight off vault.json.
fn typst_preamble_folder(root: &Path) -> String {
    let configured = json_settings(root)
        .and_then(|v| v.get("typstPreambles")?.get("folder")?.as_str().map(str::to_string));
    let Some(raw) = configured else { return DEFAULT_TYPST_PREAMBLE_FOLDER.into() };
    let trimmed = raw.trim();
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
    !name.is_empty() && parts[..parts.len().saturating_sub(1)].iter().any(|p| p.trim().to_lowercase() == name)
}

fn ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn read_meta(root: &Path, abs: &Path, folder: &str) -> Result<NoteMeta, String> {
    let md = fs::metadata(abs).map_err(|e| read_error(&e, abs))?;
    let body = read_text(abs).unwrap_or_default();
    let rel = rel_of(root, abs);
    let updated_at = ms(md.modified());
    let is_preamble = is_typst_preamble_path(&rel, &typst_preamble_folder(root));
    let title = abs.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(NoteMeta {
        link: open_note_link(&rel),
        title,
        folder: folder.to_string(),
        created_at: read_note_created_at(root, &rel, created_at_ms(&md, updated_at)),
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
            if name.starts_with('.') || is_form_dir_name(&name) {
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
/// never inside a `.base` database folder, a dot-folder or a symlink. The one
/// walk behind `list_notes` and the task scan, so both see the same notes.
pub fn list_note_files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for folder in FOLDERS {
        let top = folder_root(root, folder);
        let is_primary_root = folder == "inbox" && top == root;
        walk_notes(folder, &top, &top, is_primary_root, &mut out);
    }
    out
}

pub fn list_notes(root: &Path) -> Vec<NoteMeta> {
    list_note_files(root).into_iter().filter_map(|(abs, folder)| read_meta(root, &abs, &folder).ok()).collect()
}

fn walk_folders(folder: &str, dir: &Path, top: &Path, is_primary_root: bool, subpath: &str, out: &mut Vec<FolderRef>) {
    for (full, name, is_dir, _) in entries(dir) {
        if !is_dir || name.starts_with('.') || is_form_dir_name(&name) {
            continue;
        }
        if is_primary_root && dir == top && hidden_root_names().contains(&name.as_str()) {
            continue;
        }
        let next = if subpath.is_empty() { name.clone() } else { format!("{subpath}/{name}") };
        out.push(FolderRef { folder: folder.to_string(), subpath: next.clone() });
        walk_folders(folder, &full, top, is_primary_root, &next, out);
    }
}

pub fn list_folders(root: &Path) -> Vec<FolderRef> {
    let mut out = Vec::new();
    for folder in FOLDERS {
        let top = folder_root(root, folder);
        let is_primary_root = folder == "inbox" && top == root;
        walk_folders(folder, &top, &top, is_primary_root, "", &mut out);
    }
    out
}

fn walk_assets(root: &Path, dir: &Path, out: &mut Vec<AssetMeta>) {
    for (full, name, is_dir, is_file) in entries(dir) {
        if name.starts_with('.') {
            continue;
        }
        if is_dir {
            walk_assets(root, &full, out);
        } else if is_file {
            let Ok(md) = fs::metadata(&full) else { continue };
            out.push(AssetMeta { path: rel_of(root, &full), name, size: md.len(), updated_at: ms(md.modified()) });
        }
    }
}

pub fn list_assets(root: &Path) -> Vec<AssetMeta> {
    let mut out = Vec::new();
    for dir in ATTACHMENTS_DIRS {
        let abs = root.join(dir);
        if abs.is_dir() {
            walk_assets(root, &abs, &mut out);
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

/* ---------- Read / write / create ----------------------------------------------- */

pub fn read_note(root: &Path, rel: &str) -> Result<NoteContent, String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let body = read_text(&abs)?;
    Ok(NoteContent { meta: read_meta(root, &abs, &folder)?, body })
}

pub fn write_note(root: &Path, rel: &str, body: &str) -> Result<NoteMeta, String> {
    let (abs, folder) = note_for_write(root, rel)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    write_text(root, &abs, body)?;
    read_meta(root, &abs, &folder)
}

/// `base`, or `base 2`, `base 3`, … — the first name free in `dir`.
fn unique_title(dir: &Path, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 1;
    while dir.join(format!("{candidate}.md")).exists() {
        n += 1;
        candidate = format!("{base} {n}");
    }
    candidate
}

static UNSAFE_TITLE_RE: LazyLock<Regex> = LazyLock::new(|| re(r#"[\\/:\x00-\x1f*?"<>|]"#));

/// Filenames must be safe on all three OSes: path separators, control
/// characters and the reserved punctuation become `-`.
fn sanitize_title(raw: &str) -> String {
    let replaced = UNSAFE_TITLE_RE.replace_all(raw, "-");
    let collapsed = WHITESPACE_RE.replace_all(&replaced, " ");
    let title: String = collapsed.trim().chars().take(200).collect();
    if title.is_empty() { "Untitled".into() } else { title }
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
    let clean = trim_slashes(subpath);
    let folder_abs = folder_root(root, folder);
    let dir = if clean.is_empty() { folder_abs } else { resolve_safe(&folder_abs, clean)? };
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let final_title = unique_title(&dir, &base);
    let abs = dir.join(format!("{final_title}.md"));
    let content = body.map(str::to_string).unwrap_or_else(|| format!("# {final_title}\n\n"));
    let rel = rel_of(root, &abs);
    remove_note_creation(root, &rel, false)?;
    // A record left by an earlier note of this name would offer a bogus review.
    remove_agent_edit_record(root, &rel, false)?;
    write_file_atomic(&abs, &content)?;
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
        if !line.trim().is_empty() {
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
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf());
    let trimmed = sanitize_title(next_title);
    let target = dir.join(format!("{trimmed}.md"));
    if target != abs {
        if target.exists() {
            let same = matches!(
                (crate::vault::sidecars::file_identity(&abs), crate::vault::sidecars::file_identity(&target)),
                (Ok(a), Ok(b)) if a == b
            );
            if !same {
                return Err(format!("A note named \"{trimmed}\" already exists in {folder}"));
            }
        }
        // Comments, the creation date and the edit record travel with the note.
        relocate_note(root, &rel_of(root, &abs), &target, || Ok(()))?;
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
    let abs = resolve_safe(root, rel)?;
    let subpath = folder_subpath_of(root, &abs);
    let target_root = folder_root(root, target);
    let dest_dir = if subpath.is_empty() { target_root } else { resolve_safe(&target_root, &subpath)? };
    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let base = abs.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dest = dest_dir.join(format!("{}.md", unique_title(&dest_dir, &base)));
    relocate_note(root, &rel_of(root, &abs), &dest, || Ok(()))?;
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

fn extension_of(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()
}

pub fn move_note(root: &Path, old_rel: &str, target_folder: &str, target_subpath: &str) -> Result<NoteMeta, String> {
    let old_abs = resolve_safe(root, old_rel)?;
    let clean = trim_slashes(target_subpath);
    let folder_abs = folder_root(root, target_folder);
    let dest_dir = if clean.is_empty() { folder_abs } else { resolve_safe(&folder_abs, clean)? };
    if old_abs.parent() == Some(dest_dir.as_path()) {
        let folder = folder_of(root, &old_abs).ok_or_else(|| format!("Note not in a known folder: {old_rel}"))?;
        return read_meta(root, &old_abs, &folder);
    }
    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let base = old_abs.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dest = dest_dir.join(format!("{}{}", unique_title(&dest_dir, &base), extension_of(&old_abs)));
    relocate_note(root, &rel_of(root, &old_abs), &dest, || Ok(()))?;
    read_meta(root, &dest, target_folder)
}

pub fn duplicate_note(root: &Path, rel: &str) -> Result<NoteMeta, String> {
    let abs = resolve_safe(root, rel)?;
    let folder = folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf());
    let base = abs.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dest = dir.join(format!("{}{}", unique_title(&dir, &format!("{base} copy")), extension_of(&abs)));
    let body = read_text(&abs)?;
    remove_note_creation(root, &rel_of(root, &dest), false)?;
    write_file_atomic(&dest, &body)?;
    read_meta(root, &dest, &folder)
}

fn remove_path(path: &Path) {
    let _ = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
}

pub fn delete_note(root: &Path, rel: &str) -> Result<(), String> {
    let abs = resolve_safe(root, rel)?;
    let note_path = rel_of(root, &abs);
    remove_path(&abs);
    // A note's discussion goes with it: left behind, it would be taken over
    // by the next note created under this name.
    let _ = fs::remove_file(comments::comments_path(root, &note_path));
    remove_note_creation(root, &note_path, false)?;
    remove_agent_edit_record(root, &note_path, false)
}

pub fn empty_trash(root: &Path) {
    let trash = folder_root(root, "trash");
    let Ok(read) = fs::read_dir(&trash) else { return };
    for entry in read.flatten() {
        remove_path(&entry.path());
    }
    let trash_rel = rel_of(root, &trash);
    if let Ok(dir) = resolve_safe(&comments::comments_root(root), &trash_rel) {
        remove_path(&dir);
    }
    let _ = remove_note_creation(root, &trash_rel, true);
    let _ = remove_agent_edit_record(root, &trash_rel, true);
}

pub fn create_folder(root: &Path, top: &str, subpath: &str) -> Result<(), String> {
    let clean = trim_slashes(subpath);
    if clean.is_empty() {
        return Err("Folder name is required".into());
    }
    let abs = resolve_safe(&folder_root(root, top), clean)?;
    fs::create_dir_all(abs).map_err(|e| format!("mkdir failed: {e}"))
}

pub fn rename_folder(root: &Path, top: &str, old_subpath: &str, new_subpath: &str) -> Result<String, String> {
    let (old_clean, new_clean) = (trim_slashes(old_subpath), trim_slashes(new_subpath));
    if old_clean.is_empty() || new_clean.is_empty() {
        return Err("Both old and new folder paths are required".into());
    }
    let folder_abs = folder_root(root, top);
    let old_abs = resolve_safe(&folder_abs, old_clean)?;
    let new_abs = resolve_safe(&folder_abs, new_clean)?;
    if new_abs == old_abs {
        return Ok(new_clean.to_string());
    }
    if new_abs.starts_with(&old_abs) {
        return Err("Cannot move a folder into itself".into());
    }
    let (old_rel, new_rel) = (rel_of(root, &old_abs), rel_of(root, &new_abs));
    let comments_root = comments::comments_root(root);
    // The folder's comments, creation dates and agent edit records move as
    // one with it; a failure puts all of them back.
    relocate_folder_trees(
        vec![
            (old_abs, new_abs),
            (resolve_safe(&comments_root, &old_rel)?, resolve_safe(&comments_root, &new_rel)?),
            (note_metadata_path(root, &old_rel, true)?, note_metadata_path(root, &new_rel, true)?),
            (agent_edit_folder_path(root, &old_rel)?, agent_edit_folder_path(root, &new_rel)?),
        ],
        || Ok(()),
    )?;
    Ok(new_clean.to_string())
}

pub fn delete_folder(root: &Path, top: &str, subpath: &str) -> Result<(), String> {
    let clean = trim_slashes(subpath);
    if clean.is_empty() {
        return Err("Cannot delete the top-level folder".into());
    }
    let abs = resolve_safe(&folder_root(root, top), clean)?;
    let rel = rel_of(root, &abs);
    remove_path(&abs);
    remove_path(&resolve_safe(&comments::comments_root(root), &rel)?);
    remove_note_creation(root, &rel, true)?;
    remove_agent_edit_record(root, &rel, true)
}

/* ---------- Text search ----------------------------------------------------------- */

fn walk_search(
    root: &Path,
    folder: &str,
    dir: &Path,
    top: &Path,
    is_primary_root: bool,
    needle: &str,
    limit: usize,
    out: &mut Vec<TextMatch>,
) {
    for (full, name, is_dir, is_file) in entries(dir) {
        if out.len() >= limit {
            return;
        }
        if is_dir {
            if name.starts_with('.') || (is_primary_root && dir == top && hidden_root_names().contains(&name.as_str())) {
                continue;
            }
            walk_search(root, folder, &full, top, is_primary_root, needle, limit, out);
            continue;
        }
        if !is_file || !is_md(&name) {
            continue;
        }
        let Ok(body) = read_text(&full) else { continue };
        let rel = rel_of(root, &full);
        let title = full.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for (i, line) in body.split('\n').enumerate() {
            if out.len() >= limit {
                break;
            }
            if line.to_lowercase().contains(needle) {
                out.push(TextMatch {
                    link: open_note_link(&rel),
                    path: rel.clone(),
                    title: title.clone(),
                    folder: folder.to_string(),
                    line_number: i + 1,
                    line_text: WHITESPACE_RE.replace_all(line, " ").trim().chars().take(220).collect(),
                });
            }
        }
    }
}

/// Case-insensitive substring search over live notes, line by line.
pub fn search_text(root: &Path, query: &str, limit: usize) -> Vec<TextMatch> {
    let trimmed = query.trim();
    let mut out = Vec::new();
    if trimmed.is_empty() {
        return out;
    }
    let needle = trimmed.to_lowercase();
    for folder in LIVE_FOLDERS {
        if out.len() >= limit {
            break;
        }
        let top = folder_root(root, folder);
        let is_primary_root = folder == "inbox" && top == root;
        walk_search(root, folder, &top, &top, is_primary_root, &needle, limit, &mut out);
    }
    out
}

/* ---------- Convenience edits ------------------------------------------------------ */

fn trim_trailing_newlines(s: &str) -> &str {
    s.trim_end_matches('\n')
}

/// The note body with `text` added after a blank line.
fn append_to_body(body: &str, text: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    let sep = if normalized.ends_with('\n') || normalized.is_empty() { "" } else { "\n" };
    let gap = if normalized.is_empty() { "" } else { "\n" };
    format!("{normalized}{sep}{gap}{}\n", trim_trailing_newlines(text))
}

/// The note body with `text` inserted at the top, below any frontmatter.
fn prepend_to_body(body: &str, text: &str) -> String {
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
    let (abs, folder) = note_for_write(root, rel)?;
    let body = read_text(&abs)?;
    write_text(root, &abs, &edit(&body))?;
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
    let (abs, folder) = note_for_write(root, rel)?;
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
        write_text(root, &abs, &next)?;
    }
    Ok(ReplaceResult { meta: read_meta(root, &abs, &folder)?, replacements })
}

/* ---------- Comments and backlinks -------------------------------------------------- */

pub fn read_note_comments(root: &Path, rel: &str) -> Vec<NoteComment> {
    comments::read_note_comments(root, rel)
}

/// Replace a note's comments; an empty list removes the sidecar.
pub fn write_note_comments(root: &Path, rel: &str, list: &[Value]) -> Result<Vec<NoteComment>, String> {
    comments::write_note_comments(root, rel, &Value::Array(list.to_vec()))
}

/// Every note that wikilinks to the note at `rel`, matched on title, never
/// counting the note itself.
pub fn backlinks(root: &Path, rel: &str) -> Result<Vec<NoteMeta>, String> {
    let abs = resolve_safe(root, rel)?;
    let rel_path = rel_of(root, &abs);
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
        assert_eq!(
            extract_tags("---\ntags: [daily, \"work\"]\n---\n#café and `#code` #x/y"),
            vec!["daily", "work", "café", "x/y"]
        );
        assert_eq!(extract_tags("---\ntags:\n  - a b\n  - c\n---\n"), vec!["a", "b", "c"]);
        assert_eq!(retitle_leading_heading("---\na: 1\n---\n\n# Old\nbody", "New"), "---\na: 1\n---\n\n# New\nbody");
        assert_eq!(retitle_leading_heading("intro\n# Old", "New"), "intro\n# Old");
    }
}
