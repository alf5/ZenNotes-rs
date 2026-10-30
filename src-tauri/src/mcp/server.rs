//! The MCP tools and their stdio server, ported from upstream
//! apps/desktop/src/mcp/server.ts. Tool names, descriptions and input schemas
//! are upstream's own, captured into tools.json by
//! tooling/mcp-golden/generate.ts and served verbatim; the handlers below
//! mirror upstream's argument checks and error wording, and
//! tooling/mcp-golden/check.ts replays a scenario against both servers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use super::{agent_edits, comments, ops, tasks};

static TOOLS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("tools.json")).expect("tools.json is a list of MCP tools")
});

/* ---------- Comment authorship ---------------------------------------- */

/// The MCP client's name from the initialize handshake ("claude-code",
/// "claude-ai", "codex-cli"), read as a display name so a comment left by an
/// assistant says who left it.
pub fn comment_author_for_client(client_name: Option<&str>) -> String {
    let raw = client_name.unwrap_or("").trim();
    if raw.is_empty() {
        return "Assistant".into();
    }
    let known = match raw.to_lowercase().as_str() {
        "claude-ai" | "claude-desktop" => Some("Claude"),
        "claude-code" => Some("Claude Code"),
        "codex-cli" | "codex" => Some("Codex"),
        _ => None,
    };
    if let Some(name) = known {
        return name.into();
    }
    raw.split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/* ---------- Argument helpers ----------------------------------------- */

type Args = Map<String, Value>;

/// JavaScript truthiness, for upstream's `args.folder ? … : …` checks.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn require_string<'a>(args: &'a Args, key: &str) -> Result<&'a str, String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s),
        _ => Err(format!("Missing required string argument: {key}")),
    }
}

fn optional_string<'a>(args: &'a Args, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn require_folder(args: &Args, key: &str) -> Result<String, String> {
    let value = require_string(args, key)?;
    if ["inbox", "quick", "archive", "trash"].contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!("{key} must be one of inbox, quick, archive, trash"))
    }
}

fn optional_boolean(args: &Args, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn optional_number(args: &Args, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(_) => Err(format!("{key} must be a number")),
    }
}

fn optional_string_array(args: &Args, key: &str) -> Result<Option<Vec<String>>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            Ok(Some(items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()))
        }
        Some(_) => Err(format!("{key} must be an array of strings")),
    }
}

fn is_true(args: &Args, key: &str) -> bool {
    args.get(key) == Some(&Value::Bool(true))
}

/// `Array.prototype.slice(0, limit)`: a negative limit counts from the end.
fn take_limit<T>(mut items: Vec<T>, limit: f64) -> Vec<T> {
    let len = items.len() as f64;
    let end = if limit < 0.0 { (len + limit.trunc()).max(0.0) } else { limit.trunc().min(len) };
    items.truncate(end as usize);
    items
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn ok() -> Result<Value, String> {
    Ok(json!({ "ok": true }))
}

/* ---------- Tool handlers --------------------------------------------- */

pub struct CallContext<'a> {
    pub root: &'a Path,
    pub author: String,
    /// Set on the vault_info call that moved the session to a new vault.
    pub previous: Option<&'a Path>,
}

fn vault_info(cx: &CallContext) -> Result<Value, String> {
    let root = cx.root;
    let location = ops::primary_notes_location(root);
    let is_root_mode = location == "root";
    let switch_note = match cx.previous {
        Some(prev) => format!(
            "The vault changed: until this call the session worked in {}. Every tool runs here now, and paths or content you got from the previous vault do not apply to this one. ",
            describe_target(prev)
        ),
        None => String::new(),
    };
    let path_notes = format!(
        "IMPORTANT: Always use the `path` returned by other tools verbatim. Never prepend `inbox/` to a path you got back from list_notes / create_note / read_note. {}",
        if is_root_mode {
            "This vault uses ROOT mode: notes for the conceptual `inbox` folder live directly at the vault root (e.g. `MyNote.md`), not under `inbox/`. The folders quick/, archive/, trash/ are real subdirectories at the root."
        } else {
            "This vault uses INBOX mode: notes for the conceptual `inbox` folder live under `inbox/` (e.g. `inbox/MyNote.md`)."
        }
    );
    let vault = root.to_string_lossy();
    Ok(json!({
        "kind": "local",
        "vaultRoot": vault,
        "primaryNotesLocation": location,
        // Where the conceptual `inbox` folder lives on disk.
        "inboxAbsolutePath": if is_root_mode { vault.to_string() } else { format!("{vault}/inbox") },
        "topFolders": ["inbox", "quick", "archive", "trash"],
        "subfolders": ops::list_folders(root),
        "notes": switch_note + &path_notes,
    }))
}

fn by_updated_desc(notes: &mut [ops::NoteMeta]) {
    notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
}

fn list_notes(args: &Args, root: &Path) -> Result<Value, String> {
    let folder = if truthy(args.get("folder")) { Some(require_folder(args, "folder")?) } else { None };
    let sub = optional_string(args, "subpath")?;
    let tag = optional_string(args, "tag")?.map(str::to_lowercase);
    let wikilink_to = optional_string(args, "wikilinkTo")?.map(str::to_lowercase);
    let since = optional_number(args, "updatedSinceMs")?;
    let limit = optional_number(args, "limit")?.unwrap_or(200.0);
    let mut notes: Vec<_> = ops::list_notes(root)
        .into_iter()
        .filter(|n| match &folder {
            Some(f) => &n.folder == f,
            None => n.folder != "trash",
        })
        .collect();
    if let Some(sub) = sub.filter(|s| !s.is_empty()) {
        // Upstream builds the prefix from the conceptual folder name.
        let prefix = format!("{}/{}/", folder.as_deref().unwrap_or(""), sub.trim_matches('/'));
        notes.retain(|n| n.path.starts_with(&prefix));
    }
    if let Some(tag) = tag.filter(|t| !t.is_empty()) {
        notes.retain(|n| n.tags.iter().any(|t| t.to_lowercase() == tag));
    }
    if let Some(target) = wikilink_to.filter(|w| !w.is_empty()) {
        notes.retain(|n| n.wikilinks.iter().any(|w| w.to_lowercase() == target));
    }
    if let Some(since) = since {
        notes.retain(|n| n.updated_at as f64 >= since);
    }
    by_updated_desc(&mut notes);
    to_value(take_limit(notes, limit))
}

fn list_tags(root: &Path) -> Result<Value, String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for note in ops::list_notes(root).into_iter().filter(|n| n.folder != "trash") {
        for tag in note.tags {
            let key = tag.to_lowercase();
            match counts.iter_mut().find(|(t, _)| *t == key) {
                Some(entry) => entry.1 += 1,
                None => counts.push((key, 1)),
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(Value::Array(counts.into_iter().map(|(tag, count)| json!({ "tag": tag, "count": count })).collect()))
}

fn list_tasks(args: &Args, root: &Path) -> Result<Value, String> {
    let status = optional_string(args, "status")?.unwrap_or("open").to_string();
    let include_excluded = is_true(args, "includeExcluded");
    let priority = optional_string(args, "priority")?;
    let due_before = optional_string(args, "dueBefore")?;
    let due_after = optional_string(args, "dueAfter")?;
    let tag = optional_string(args, "tag")?.map(str::to_lowercase);
    let tags: Option<Vec<String>> =
        optional_string_array(args, "tags")?.map(|t| t.iter().map(|x| x.to_lowercase()).collect());
    let folder = if truthy(args.get("folder")) { Some(require_folder(args, "folder")?) } else { None };
    let all = to_value(tasks::scan_all_tasks(root, include_excluded))?;
    let flag = |t: &Value, key: &str| t.get(key).and_then(Value::as_bool).unwrap_or(false);
    let text = |t: &Value, key: &str| t.get(key).and_then(Value::as_str).map(str::to_string);
    let filtered = all.as_array().cloned().unwrap_or_default().into_iter().filter(|t| {
        let task_tags: Vec<String> = t
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_lowercase).collect())
            .unwrap_or_default();
        let due = text(t, "due");
        if folder.as_ref().is_some_and(|f| text(t, "noteFolder").as_ref() != Some(f)) {
            return false;
        }
        // A `[>]` record's live copy sits in the destination note, and a `[-]`
        // cancelled task is abandoned: neither is open work.
        if status == "open"
            && (flag(t, "checked") || flag(t, "waiting") || flag(t, "forwarded") || flag(t, "cancelled"))
        {
            return false;
        }
        if status == "done" && !flag(t, "checked") {
            return false;
        }
        if status == "waiting" && !flag(t, "waiting") {
            return false;
        }
        if priority.is_some_and(|p| !p.is_empty() && text(t, "priority").as_deref() != Some(p)) {
            return false;
        }
        if let Some(before) = due_before.filter(|d| !d.is_empty()) {
            if due.as_deref().is_none_or(|d| d.is_empty() || d >= before) {
                return false;
            }
        }
        if let Some(after) = due_after.filter(|d| !d.is_empty()) {
            if due.as_deref().is_none_or(|d| d.is_empty() || d < after) {
                return false;
            }
        }
        if tag.as_ref().is_some_and(|tag| !tag.is_empty() && !task_tags.contains(tag)) {
            return false;
        }
        if tags.as_ref().is_some_and(|need| !need.iter().all(|n| task_tags.contains(n))) {
            return false;
        }
        true
    });
    Ok(Value::Array(filtered.collect()))
}

/// Run one tool against the vault at `cx.root`. `Value::String` results are
/// sent as plain text, everything else as pretty JSON, as upstream does.
pub fn call_tool(name: &str, args: &Args, cx: &CallContext) -> Result<Value, String> {
    let root = cx.root;
    let path = || require_string(args, "path");
    match name {
        "vault_info" => vault_info(cx),
        "list_notes" => list_notes(args, root),
        "list_folders" => to_value(ops::list_folders(root)),
        "list_assets" => to_value(ops::list_assets(root)),
        "read_note" => to_value(ops::read_note(root, path()?)?),
        "write_note" => {
            let rel = path()?;
            let body = args.get("body").and_then(Value::as_str).ok_or("body must be a string")?;
            to_value(ops::write_note(root, rel, body)?)
        }
        "create_note" => {
            let folder = if truthy(args.get("folder")) { require_folder(args, "folder")? } else { "inbox".into() };
            if folder == "trash" {
                return Err("Refusing to create a note directly in trash/".into());
            }
            let title = optional_string(args, "title")?;
            let subpath = optional_string(args, "subpath")?.unwrap_or("");
            let body = optional_string(args, "body")?;
            to_value(ops::create_note(root, &folder, title, subpath, body)?)
        }
        "rename_note" => {
            let rel = path()?;
            to_value(ops::rename_note(root, rel, require_string(args, "newTitle")?)?)
        }
        "move_note" => {
            let rel = path()?;
            let folder = require_folder(args, "targetFolder")?;
            let sub = optional_string(args, "targetSubpath")?.unwrap_or("");
            to_value(ops::move_note(root, rel, &folder, sub)?)
        }
        "duplicate_note" => to_value(ops::duplicate_note(root, path()?)?),
        "move_to_trash" => to_value(ops::move_to_trash(root, path()?)?),
        "restore_from_trash" => to_value(ops::restore_from_trash(root, path()?)?),
        "empty_trash" => {
            if !is_true(args, "confirm") {
                return Err("empty_trash requires confirm=true. Ask the user first.".into());
            }
            ops::empty_trash(root);
            ok()
        }
        "delete_note" => {
            if !is_true(args, "confirm") {
                return Err("delete_note requires confirm=true.".into());
            }
            ops::delete_note(root, path()?)?;
            ok()
        }
        "archive_note" => to_value(ops::archive_note(root, path()?)?),
        "unarchive_note" => to_value(ops::unarchive_note(root, path()?)?),
        "create_folder" => {
            let folder = require_folder(args, "folder")?;
            if folder == "trash" {
                return Err("Refusing to create subfolders inside trash/".into());
            }
            ops::create_folder(root, &folder, require_string(args, "subpath")?)?;
            ok()
        }
        "rename_folder" => {
            let folder = require_folder(args, "folder")?;
            if folder == "trash" {
                return Err("Cannot rename inside trash/".into());
            }
            let old = require_string(args, "oldSubpath")?;
            let new = require_string(args, "newSubpath")?;
            Ok(Value::String(ops::rename_folder(root, &folder, old, new)?))
        }
        "delete_folder" => {
            if !is_true(args, "confirm") {
                return Err("delete_folder requires confirm=true.".into());
            }
            let folder = require_folder(args, "folder")?;
            if folder == "trash" {
                return Err("Cannot delete inside trash/".into());
            }
            ops::delete_folder(root, &folder, require_string(args, "subpath")?)?;
            ok()
        }
        "search_text" => {
            let query = require_string(args, "query")?;
            let limit = optional_number(args, "limit")?.unwrap_or(80.0);
            to_value(ops::search_text(root, query, limit.max(0.0) as usize))
        }
        "search_by_title" => {
            let needle = require_string(args, "query")?.to_lowercase();
            let limit = optional_number(args, "limit")?.unwrap_or(20.0);
            let mut notes: Vec<_> = ops::list_notes(root)
                .into_iter()
                .filter(|n| n.folder != "trash" && n.title.to_lowercase().contains(&needle))
                .collect();
            by_updated_desc(&mut notes);
            to_value(take_limit(notes, limit))
        }
        "search_by_tag" => {
            let raw = require_string(args, "tag")?;
            let tag = raw.strip_prefix('#').unwrap_or(raw).to_lowercase();
            let limit = optional_number(args, "limit")?.unwrap_or(200.0);
            let mut notes: Vec<_> = ops::list_notes(root)
                .into_iter()
                .filter(|n| n.folder != "trash" && n.tags.iter().any(|t| t.to_lowercase() == tag))
                .collect();
            by_updated_desc(&mut notes);
            to_value(take_limit(notes, limit))
        }
        "list_tags" => list_tags(root),
        "backlinks" => to_value(ops::backlinks(root, path()?)?),
        "list_tasks" => list_tasks(args, root),
        "toggle_task" => to_value(tasks::toggle_task(root, require_string(args, "id")?)?),
        "append_to_note" => {
            let rel = path()?;
            to_value(ops::append_to_note(root, rel, require_string(args, "text")?)?)
        }
        "prepend_to_note" => {
            let rel = path()?;
            to_value(ops::prepend_to_note(root, rel, require_string(args, "text")?)?)
        }
        "insert_at_line" => {
            let rel = path()?;
            let line = optional_number(args, "lineNumber")?.ok_or("lineNumber is required")?;
            to_value(ops::insert_at_line(root, rel, line, require_string(args, "text")?)?)
        }
        "replace_in_note" => {
            let rel = path()?;
            let find = require_string(args, "find")?;
            let replace = args.get("replace").and_then(Value::as_str).ok_or("replace must be a string")?;
            let all = optional_string(args, "occurrence")? == Some("all");
            to_value(ops::replace_in_note(root, rel, find, replace, all)?)
        }
        "list_comments" => {
            let include = optional_boolean(args, "include_resolved")?.unwrap_or(false);
            to_value(comments::list_threads(root, path()?, include)?)
        }
        "add_comment" => {
            let rel = path()?;
            let body = require_string(args, "body")?;
            let anchor = optional_string(args, "anchor_text")?;
            let author = optional_string(args, "author")?.map(str::to_string).unwrap_or_else(|| cx.author.clone());
            to_value(comments::add_comment(root, rel, body, anchor, &author)?)
        }
        "reply_to_comment" => {
            let rel = path()?;
            let id = require_string(args, "id")?;
            let body = require_string(args, "body")?;
            let author = optional_string(args, "author")?.map(str::to_string).unwrap_or_else(|| cx.author.clone());
            to_value(comments::reply_to_comment(root, rel, id, body, &author)?)
        }
        "resolve_comment" => {
            let rel = path()?;
            let id = require_string(args, "id")?;
            let resolved = optional_boolean(args, "resolved")?.unwrap_or(true);
            to_value(comments::resolve_comment(root, rel, id, resolved)?)
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

/* ---------- Vault session --------------------------------------------- */

fn describe_target(root: &Path) -> String {
    format!("the local vault {}", root.display())
}

/// Whether two roots name one vault: the same path, or the same directory
/// reached two ways.
fn same_vault(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// The vault the tools run against, resolved again before every call so the
/// server follows the desktop app the way a fresh `zn` does. A switch never
/// redirects work silently: an agent that read notes in one vault would
/// otherwise write to a note at the same path in the other, so once the vault
/// changes, every call except vault_info fails until vault_info confirms it.
pub struct VaultSession {
    resolve: Box<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    current: Option<PathBuf>,
}

impl VaultSession {
    pub fn new(resolve: impl Fn() -> Result<PathBuf, String> + Send + Sync + 'static) -> Self {
        Self { resolve: Box::new(resolve), current: None }
    }

    /// The root for one call and, for the vault_info that moved the session,
    /// the root it left.
    pub fn root_for(&mut self, confirm: bool) -> Result<(PathBuf, Option<PathBuf>), String> {
        let target = (self.resolve)()?;
        let mut previous = None;
        if let Some(current) = &self.current {
            if !same_vault(current, &target) {
                if !confirm {
                    return Err(format!(
                        "The ZenNotes vault changed since this session's last call: it was {} and is now {}. Nothing ran. Call vault_info to confirm the new vault and tell the user, then re-read anything you meant to change before you retry.",
                        describe_target(current),
                        describe_target(&target)
                    ));
                }
                previous = Some(current.clone());
            }
        }
        self.current = Some(target.clone());
        Ok((target, previous))
    }
}

/* ---------- Server plumbing ------------------------------------------- */

pub struct ZenNotesServer {
    /// One tool call at a time: a batch of parallel calls sees one consistent
    /// session, and two edits to a note never race on its review record.
    session: Arc<Mutex<VaultSession>>,
    instructions: String,
}

impl ZenNotesServer {
    pub fn new(session: VaultSession, instructions: String) -> Self {
        Self { session: Arc::new(Mutex::new(session)), instructions }
    }
}

fn text_result(payload: String, is_error: bool) -> CallToolResponse {
    let content = vec![ContentBlock::text(payload)];
    if is_error { CallToolResult::error(content) } else { CallToolResult::success(content) }.into()
}

impl ServerHandler for ZenNotesServer {
    fn get_info(&self) -> InitializeResult {
        let mut info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("zennotes", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(self.instructions.clone());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut result = ListToolsResult::default();
        result.tools = TOOLS.clone();
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if !TOOLS.iter().any(|t| t.name == name) {
            return Ok(text_result(format!("Unknown tool: {name}"), true));
        }
        let args = request.arguments.unwrap_or_default();
        let client = context.peer.peer_info().map(|info| info.client_info.name.clone());
        let author = comment_author_for_client(client.as_deref());
        let session = self.session.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut session = session.blocking_lock();
            let (root, previous) = session.root_for(name == "vault_info")?;
            let cx = CallContext { root: &root, author: author.clone(), previous: previous.as_deref() };
            // An edit to a note is recorded so the user can review it in the app.
            agent_edits::run_recording(&name, &args, &root, &author, || call_tool(&name, &args, &cx))
        })
        .await
        .unwrap_or_else(|e| Err(format!("tool panicked: {e}")));
        Ok(match outcome {
            Ok(Value::String(text)) => text_result(text, false),
            Ok(value) => text_result(serde_json::to_string_pretty(&value).unwrap_or_default(), false),
            Err(message) => text_result(format!("Error: {message}"), true),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authors_read_like_upstream() {
        assert_eq!(comment_author_for_client(Some("claude-code")), "Claude Code");
        assert_eq!(comment_author_for_client(Some("Claude-AI")), "Claude");
        assert_eq!(comment_author_for_client(Some("my_cool-agent")), "My Cool Agent");
        assert_eq!(comment_author_for_client(None), "Assistant");
    }

    #[test]
    fn tools_json_names_every_handler() {
        let dir = tempfile::tempdir().unwrap();
        let cx = CallContext { root: dir.path(), author: "A".into(), previous: None };
        for name in TOOLS.iter().map(|t| t.name.to_string()) {
            let result = call_tool(&name, &Map::new(), &cx);
            assert_ne!(result, Err(format!("Unknown tool: {name}")), "{name} has no handler");
        }
    }

    #[test]
    fn slices_like_javascript() {
        assert_eq!(take_limit(vec![1, 2, 3], 2.0), vec![1, 2]);
        assert_eq!(take_limit(vec![1, 2, 3], -1.0), vec![1, 2]);
        assert_eq!(take_limit(vec![1, 2, 3], 9.5), vec![1, 2, 3]);
    }
}
