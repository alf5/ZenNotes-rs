//! Vault tasks for the MCP `list_tasks` / `toggle_task` tools — port of the
//! task section of upstream apps/desktop/src/mcp/vault-ops.ts (`VaultTask`,
//! `parseTasksFromBody`, `parseTaskFile`, `scanAllTasks`, `toggleTask`,
//! `splitTaskId`, `toggleTaskInBody`, `toggleFileTaskInBody` and the helpers
//! under them), plus the shared-domain pieces it imports: `inlineTaskField`
//! and `noteTasksMode` (packages/shared-domain/src/tasks.ts) and the folder
//! lists of packages/shared-domain/src/tasks-excluded-folders.ts.
//!
//! The MCP server keeps its OWN copy of the task grammar rather than the app's
//! (`@shared/tasklists`): it is what agents have always been handed, field for
//! field, so this ports that copy and not the renderer's parser.
//!
//! Every regex is a JS regex re-spelled for Rust, and the spelling is where the
//! port can drift, so the rules are kept in one place:
//! - JS `\s` (and `String.prototype.trim`) is ECMAScript WhiteSpace plus
//!   LineTerminator, which is NOT Unicode White_Space: it has U+FEFF and lacks
//!   U+0085. Hence `WS` / `is_js_ws` below instead of Rust's `\s` / `trim`.
//! - JS `\d` is ASCII (with or without `u`), Rust's is Unicode: `[0-9]`.
//! - JS `.` stops at `\r`, U+2028 and U+2029 as well as `\n`: `DOT`.
//! - A JS `\b` without the `u` flag is an ASCII word boundary; every `\b` here
//!   follows a letter, so it becomes `(?![A-Za-z0-9_])`, kept outside the
//!   `(?i:…)` group so case folding cannot widen it to U+212A / U+017F.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use fancy_regex::Regex;
use serde::Serialize;

use super::links::open_note_link;
use crate::vault::atomic::write_file_atomic;
use crate::vault::layout::{INTERNAL_VAULT_DIR, VAULT_SETTINGS_FILE};
use crate::vault::notes::{resolve_safe, title_from_path};
use crate::vault::sidecars::{prepare_note_creation, rel_of};

/// One task as the MCP tools return it. Field order is the key order of the
/// object literals upstream builds (`parseTasksFromBody` puts `inProgress`
/// before `forwarded`, unlike the interface), so `serde_json` emits the same
/// JSON `JSON.stringify` does. The `Option` fields are the ones upstream can
/// leave `undefined`, which `JSON.stringify` drops.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VaultTask {
    pub id: String,
    pub source_path: String,
    /// Deep link to the source note; same contract as a note's `link`.
    pub link: String,
    pub note_title: String,
    pub note_folder: String,
    /// Zero-based line of the checkbox (0 for a file task).
    pub line_number: usize,
    /// Position among the note's checkboxes; -1 for a file task.
    pub task_index: i64,
    pub raw_text: String,
    pub content: String,
    pub checked: bool,
    /// `[-]` cancelled (#450). Always set by both parsers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
    /// `[/]` in progress (#512). Always set by both parsers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress: Option<bool>,
    /// `[>]` forwarded record (#316). Inline tasks only: a file task has no
    /// such state and upstream leaves the key out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// `high` | `med` | `low`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    pub waiting: bool,
    /// Kanban status column, lower-cased (ZenNotes/zennotes#941).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub tags: Vec<String>,
    /// `file` for a whole-note task; absent (inline) for a checkbox line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_date: Option<String>,
}

/// The note a body came from: its vault-relative POSIX path, its title and
/// its top-level folder (`inbox` | `quick` | `archive` | `trash`).
pub struct TaskContext<'a> {
    pub path: &'a str,
    pub title: &'a str,
    pub folder: &'a str,
}

/* ---------- JS regex semantics ---------------------------------------- */

/// ECMAScript `\s`, as a character-class body.
macro_rules! ws {
    () => {
        r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}"
    };
}
/// ECMAScript `.` without the `s` flag.
macro_rules! dot {
    () => {
        r"[^\n\r\x{2028}\x{2029}]"
    };
}

fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
            | '\r'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

/// `s.replace(/\s+/g, ' ')`.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for c in s.chars() {
        if is_js_ws(c) {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static task regex")
}

/// `/^(\s{0,3})(`{3,}|~{3,})/` — group 1 is the fence marker.
static FENCE_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^[", ws!(), "]{0,3}(`{3,}|~{3,})")));
/// `/^\s*(?:>\s*)*(?:[-*+]|\d+[.)])\s+\[([ xX>/-])\](.*)$/`: the task lines
/// `@shared/tasklists` matches — bullets, ordered lists, quoted tasks, and
/// every state the app knows. Groups: the state char, then the tail.
static TASK_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        "^[", ws!(), "]*(?:>[", ws!(), "]*)*(?:[-*+]|[0-9]+[.)])[", ws!(), r"]+\[([ xX>/-])\](",
        dot!(),
        "*)$"
    ))
});
/// `/^---\r?\n([\s\S]*?)\r?\n---\r?\n?/`.
static FRONTMATTER_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^---\r?\n([\s\S]*?)\r?\n---\r?\n?"));
/// `/(?:^|\s)due:\s*(\S+)/i` — optional space after the colon (#343).
static INLINE_DUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!("(?:^|[", ws!(), "])(?i:due):[", ws!(), "]*([^", ws!(), "]+)"))
});
/// `/(?:^|\s)!(high|med|medium|low|h|m|l)\b/i`. Alternation order matters:
/// `!medium` first tries `med`, fails the boundary, then backtracks.
static INLINE_PRIORITY_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!("(?:^|[", ws!(), "])!((?i:high|med|medium|low|h|m|l))(?![A-Za-z0-9_])"))
});
/// `/(?:^|\s)@waiting\b/i`.
static INLINE_WAITING_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("(?:^|[", ws!(), "])@(?i:waiting)(?![A-Za-z0-9_])")));
/// `/(?:^|\s)#([\p{L}\d][\p{L}\d/_-]*)/gu` — tags in any script (#205).
static INLINE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!(r"(?:^|[", ws!(), r"])#([\p{L}0-9][\p{L}0-9/_-]*)")));
/// `/(?:^|\s)@([a-z][a-z0-9_-]*):([\p{L}\d][\p{L}\d/_-]*)/giu` (#354). Rust's
/// `(?i)` folds `[a-z]` the way JS `iu` does (U+017F and U+212A included).
static INLINE_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(r"(?i)(?:^|[", ws!(), r"])@([a-z][a-z0-9_-]*):([\p{L}0-9][\p{L}0-9/_-]*)"))
});
/// `/^\d{4}-\d{2}-\d{2}$/`.
static ISO_DATE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$"));
/// `/^\s*-\s+(.*)$/` — a block-list item line.
static FM_ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^[", ws!(), "]*-[", ws!(), "]+(", dot!(), "*)$")));
/// `/^([A-Za-z0-9_][\w-]*)\s*:\s*(.*)$/` — a top-level `key: value` line.
static FM_KV_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!("^([A-Za-z0-9_][A-Za-z0-9_-]*)[", ws!(), "]*:[", ws!(), "]*(", dot!(), "*)$"))
});
/// `/^([A-Za-z0-9_][\w-]*)\s*:/` — the key `setFrontmatterScalar` looks for.
static FM_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^([A-Za-z0-9_][A-Za-z0-9_-]*)[", ws!(), "]*:")));

/// Group `i` of the first match, or `None`. These patterns are linear and
/// bounded, so fancy-regex's backtrack limit is never the reason for a miss.
fn captures<'t>(regex: &Regex, text: &'t str) -> Option<fancy_regex::Captures<'t>> {
    regex.captures(text).ok().flatten()
}

fn group<'t>(caps: &fancy_regex::Captures<'t>, i: usize) -> &'t str {
    caps.get(i).map(|m| m.as_str()).unwrap_or("")
}

/// `s.replace(regex, ' ')` — non-global: the first match only.
fn replace_first_with_space(regex: &Regex, s: &str) -> String {
    match regex.find(s).ok().flatten() {
        Some(m) => format!("{} {}", &s[..m.start()], &s[m.end()..]),
        None => s.to_string(),
    }
}

/* ---------- Scalar helpers -------------------------------------------- */

fn unquote(v: &str) -> &str {
    let trimmed = js_trim(v);
    let bytes = trimmed.as_bytes();
    // Both ends are one-byte ASCII quotes, so byte length 2 is UTF-16 length 2.
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            return &trimmed[1..trimmed.len() - 1];
        }
    }
    trimmed
}

fn normalize_priority(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|r| !r.is_empty())?;
    let lower = raw.to_lowercase();
    match js_trim(&lower) {
        "high" | "h" => Some("high".into()),
        // `normal` is the TaskNotes default priority; ZenNotes calls it `med`.
        "med" | "medium" | "normal" | "m" => Some("med".into()),
        "low" | "l" => Some("low".into()),
        _ => None,
    }
}

/// The shape check plus `Number.isFinite(Date.parse(`${s}T00:00:00Z`))`.
/// Both V8 and JSC accept any day 01-31 in any month 01-12 there (Feb 30 is
/// "valid" and rolls over), so that, not a calendar check, is the rule.
fn is_valid_iso_date(s: &str) -> bool {
    if !ISO_DATE_RE.is_match(s).unwrap_or(false) {
        return false;
    }
    let month: u32 = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn normalize_due_date(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|r| !r.is_empty())?;
    let cleaned = unquote(js_trim(raw));
    is_valid_iso_date(cleaned).then(|| cleaned.to_string())
}

/// A note's frontmatter `tasks:` switch (#458). `NoteTasksMode` upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteTasksMode {
    All,
    NoteOnly,
    None,
}

/// `noteTasksMode` (shared-domain tasks.ts). No runtime types YAML scalars,
/// so `tasks: false` is the string "false"; a list contributes its first item.
fn note_tasks_mode(value: Option<&str>) -> NoteTasksMode {
    match value.map(|v| js_trim(v).to_lowercase()).as_deref() {
        Some("false") | Some("off") => NoteTasksMode::None,
        Some("note") => NoteTasksMode::NoteOnly,
        _ => NoteTasksMode::All,
    }
}

/// `inlineTaskField` (shared-domain tasks.ts): the first `@<key>:<value>` the
/// text carries for `key`, lower-cased.
fn inline_task_field(text: &str, key: &str) -> Option<String> {
    let wanted = key.to_lowercase();
    INLINE_FIELD_RE
        .captures_iter(text)
        .filter_map(Result::ok)
        .find(|caps| group(caps, 1).to_lowercase() == wanted)
        .map(|caps| group(&caps, 2).to_lowercase())
}

/* ---------- Inline tasks ---------------------------------------------- */

#[derive(Default)]
struct NoteDefaults {
    due: Option<String>,
    priority: Option<String>,
    status: Option<String>,
    tasks_mode: Option<NoteTasksMode>,
}

/// vault-ops' own `parseNoteDefaults`, which differs from the shared one: it
/// strips quotes with `/^["']|["']$/g` (one at each end, independently) and
/// keeps `status:` only when it is non-empty.
fn parse_note_defaults(body: &str) -> NoteDefaults {
    let mut out = NoteDefaults::default();
    let Some(caps) = captures(&FRONTMATTER_RE, body) else {
        return out;
    };
    for raw_line in group(&caps, 1).split('\n') {
        let line = js_trim(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else { continue };
        if colon < 1 {
            continue;
        }
        let key = js_trim(&line[..colon]).to_lowercase();
        let mut value = js_trim(&line[colon + 1..]);
        if let Some(rest) = value.strip_prefix(['"', '\'']) {
            value = rest;
        }
        if let Some(rest) = value.strip_suffix(['"', '\'']) {
            value = rest;
        }
        match key.as_str() {
            "due" if is_valid_iso_date(value) => out.due = Some(value.to_string()),
            "priority" => {
                if let Some(p) = normalize_priority(Some(value)) {
                    out.priority = Some(p);
                }
            }
            "status" => {
                if !value.is_empty() {
                    out.status = Some(value.to_lowercase());
                }
            }
            "tasks" => out.tasks_mode = Some(note_tasks_mode(Some(value))),
            _ => {}
        }
    }
    out
}

/// Fence tracking shared by the parser and the toggle, so both count the same
/// checkboxes: a fence closes only on the marker that opened it, verbatim.
struct FenceState {
    marker: Option<String>,
}

impl FenceState {
    /// Whether `line` is fenced code or a fence line (either way, not a task).
    fn skip(&mut self, line: &str) -> bool {
        if let Some(caps) = captures(&FENCE_LINE_RE, line) {
            let marker = group(&caps, 1);
            match &self.marker {
                None => self.marker = Some(marker.to_string()),
                Some(open) if open == marker => self.marker = None,
                Some(_) => {}
            }
            return true;
        }
        self.marker.is_some()
    }
}

/// `parseTasksFromBody`: every `- [ ]`-style checkbox outside fenced code.
/// `include_excluded` scans past the note's `tasks:` opt-out (#458).
pub fn parse_tasks_from_body(
    body: &str,
    ctx: &TaskContext,
    include_excluded: bool,
) -> Vec<VaultTask> {
    let normalized = body.replace("\r\n", "\n");
    let defaults = parse_note_defaults(&normalized);
    // `tasks: false` and `tasks: note` both silence inline checkboxes.
    if defaults.tasks_mode.is_some_and(|m| m != NoteTasksMode::All) && !include_excluded {
        return Vec::new();
    }
    let mut tasks = Vec::new();
    let mut fence = FenceState { marker: None };
    let mut task_index: i64 = 0;
    for (i, line) in normalized.split('\n').enumerate() {
        if fence.skip(line) {
            continue;
        }
        let Some(caps) = captures(&TASK_LINE_RE, line) else { continue };
        let state = group(&caps, 1);
        let tail = group(&caps, 2);

        let mut due = None;
        let mut priority = None;
        let mut waiting = false;
        let mut stripped = tail.to_string();
        if let Some(m) = captures(&INLINE_DUE_RE, &stripped) {
            let value = group(&m, 1);
            if is_valid_iso_date(value) {
                due = Some(value.to_string());
            }
            stripped = replace_first_with_space(&INLINE_DUE_RE, &stripped);
        }
        if let Some(m) = captures(&INLINE_PRIORITY_RE, &stripped) {
            priority = normalize_priority(Some(group(&m, 1)));
            stripped = replace_first_with_space(&INLINE_PRIORITY_RE, &stripped);
        }
        if INLINE_WAITING_RE.is_match(&stripped).unwrap_or(false) {
            waiting = true;
            stripped = replace_first_with_space(&INLINE_WAITING_RE, &stripped);
        }
        // Tags come from the original tail, not the stripped one.
        let mut tags: Vec<String> = Vec::new();
        for caps in INLINE_TAG_RE.captures_iter(tail).filter_map(Result::ok) {
            let tag = group(&caps, 1).to_lowercase();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        let collapsed = collapse_ws(&stripped);
        let content = match js_trim(&collapsed) {
            "" => js_trim(tail),
            c => c,
        };

        tasks.push(VaultTask {
            id: format!("{}#{task_index}", ctx.path),
            source_path: ctx.path.to_string(),
            link: open_note_link(ctx.path),
            note_title: ctx.title.to_string(),
            note_folder: ctx.folder.to_string(),
            line_number: i,
            task_index,
            raw_text: line.to_string(),
            content: content.to_string(),
            checked: state == "x" || state == "X",
            cancelled: Some(state == "-"),
            in_progress: Some(state == "/"),
            forwarded: Some(state == ">"),
            due: due.or_else(|| defaults.due.clone()),
            priority: priority.or_else(|| defaults.priority.clone()),
            waiting,
            status: inline_task_field(tail, "status").or_else(|| defaults.status.clone()),
            tags,
            kind: None,
            scheduled: None,
            completed_date: None,
        });
        task_index += 1;
    }
    tasks
}

/* ---------- File tasks (TaskNotes-style: one task per note) ----------- */

/// The frontmatter tag that marks a whole note as a task.
const TASK_FILE_TAG: &str = "task";
/// `status:` values read as done (checked).
const DONE_STATUSES: [&str; 4] = ["done", "complete", "completed", "x"];
/// `status:` values read as cancelled (#450).
const CANCELLED_STATUSES: [&str; 2] = ["cancelled", "canceled"];
/// `status:` values read as in progress (#512) — still open work.
const IN_PROGRESS_STATUSES: [&str; 6] =
    ["in-progress", "in progress", "inprogress", "doing", "started", "wip"];

enum FmValue {
    Scalar(String),
    List(Vec<String>),
}

/// `firstScalar`: a scalar, or a list's first item.
fn first_scalar(v: Option<&FmValue>) -> Option<&str> {
    match v? {
        FmValue::Scalar(s) => Some(s),
        FmValue::List(items) => items.first().map(String::as_str),
    }
}

/// `asArray`.
fn as_array(v: Option<&FmValue>) -> Vec<&str> {
    match v {
        None => Vec::new(),
        Some(FmValue::Scalar(s)) => vec![s.as_str()],
        Some(FmValue::List(items)) => items.iter().map(String::as_str).collect(),
    }
}

/// `parseTaskFrontmatter`: scalars, inline arrays and block lists, keys
/// lower-cased. Just enough YAML for task files; never fails.
fn parse_task_frontmatter(block: &str) -> HashMap<String, FmValue> {
    let mut data: HashMap<String, FmValue> = HashMap::new();
    let mut list_key: Option<String> = None;
    for raw_line in block.split('\n') {
        let trimmed = js_trim(raw_line);
        // Blank and comment lines leave an open block list open.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(key) = &list_key {
            if raw_line.starts_with(is_js_ws) {
                if let Some(item) = captures(&FM_ITEM_RE, raw_line) {
                    if let Some(FmValue::List(items)) = data.get_mut(key) {
                        items.push(unquote(group(&item, 1)).to_string());
                    }
                    continue;
                }
            }
        }
        let Some(kv) = captures(&FM_KV_RE, raw_line) else {
            list_key = None;
            continue;
        };
        let key = group(&kv, 1).to_lowercase();
        let rest = js_trim(group(&kv, 2));
        if rest.is_empty() {
            // Bare key: a block list may follow on indented `- item` lines.
            data.insert(key.clone(), FmValue::List(Vec::new()));
            list_key = Some(key);
            continue;
        }
        list_key = None;
        let value = if rest.starts_with('[') && rest.ends_with(']') && rest.len() >= 2 {
            FmValue::List(
                rest[1..rest.len() - 1]
                    .split(',')
                    .map(|s| unquote(s).to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        } else {
            FmValue::Scalar(unquote(rest).to_string())
        };
        data.insert(key, value);
    }
    data
}

/// `parseTaskFile`: the whole-note task a note tagged `task` is, or `None`.
/// Emitted in addition to the note's inline checkboxes.
pub fn parse_task_file(body: &str, ctx: &TaskContext, include_excluded: bool) -> Option<VaultTask> {
    let normalized = body.replace("\r\n", "\n");
    let caps = captures(&FRONTMATTER_RE, &normalized)?;
    let fm = parse_task_frontmatter(group(&caps, 1));

    // `tasks: false` wins over `tags: [task]`; `tasks: note` deliberately falls
    // through, keeping the file task while the checkboxes drop. (#458)
    if note_tasks_mode(first_scalar(fm.get("tasks"))) == NoteTasksMode::None && !include_excluded {
        return None;
    }
    let tags: Vec<String> = as_array(fm.get("tags"))
        .into_iter()
        .map(|t| t.strip_prefix('#').unwrap_or(t).to_lowercase())
        .collect();
    if !tags.iter().any(|t| t == TASK_FILE_TAG) {
        return None;
    }
    let status = first_scalar(fm.get("status")).unwrap_or("open").to_lowercase();
    let title = match first_scalar(fm.get("title")).map(js_trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => ctx.title.to_string(),
    };

    Some(VaultTask {
        id: format!("{}#task", ctx.path),
        source_path: ctx.path.to_string(),
        link: open_note_link(ctx.path),
        note_title: ctx.title.to_string(),
        note_folder: ctx.folder.to_string(),
        line_number: 0,
        task_index: -1,
        raw_text: String::new(),
        content: title,
        checked: DONE_STATUSES.contains(&status.as_str()),
        cancelled: Some(CANCELLED_STATUSES.contains(&status.as_str())),
        in_progress: Some(IN_PROGRESS_STATUSES.contains(&status.as_str())),
        forwarded: None,
        due: normalize_due_date(first_scalar(fm.get("due"))),
        priority: normalize_priority(first_scalar(fm.get("priority"))),
        waiting: status == "waiting",
        tags: tags.into_iter().filter(|t| t != TASK_FILE_TAG).collect(),
        status: Some(status),
        kind: Some("file".into()),
        scheduled: normalize_due_date(first_scalar(fm.get("scheduled"))),
        completed_date: normalize_due_date(first_scalar(fm.get("completeddate"))),
    })
}

/// `setFrontmatterScalar`: add, update or (`None`) remove a top-level
/// `key: value` line, leaving every other line alone; creates the block when
/// there is none. Returns `\n`-normalized text, like upstream.
fn set_frontmatter_scalar(body: &str, key: &str, value: Option<&str>) -> String {
    let normalized = body.replace("\r\n", "\n");
    let lower_key = key.to_lowercase();
    let Some(caps) = captures(&FRONTMATTER_RE, &normalized) else {
        return match value {
            None => normalized,
            Some(v) => format!("---\n{key}: {v}\n---\n{normalized}"),
        };
    };
    let rest = &normalized[caps.get(0).map_or(0, |m| m.end())..];
    let mut lines: Vec<String> = group(&caps, 1).split('\n').map(str::to_string).collect();
    let idx = lines.iter().position(|line| {
        captures(&FM_KEY_RE, line).is_some_and(|kv| group(&kv, 1).to_lowercase() == lower_key)
    });
    match (value, idx) {
        (None, Some(i)) => {
            lines.remove(i);
        }
        (None, None) => {}
        (Some(v), Some(i)) => lines[i] = format!("{key}: {v}"),
        (Some(v), None) => lines.push(format!("{key}: {v}")),
    }
    format!("---\n{}\n---\n{rest}", lines.join("\n"))
}

/// `todayIsoLocal`: today as a local `YYYY-MM-DD`.
fn today_iso_local() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// `toggleFileTaskInBody`: reopen a done file task, or mark an open one done
/// with today's `completedDate`.
fn toggle_file_task_in_body(body: &str, currently_checked: bool) -> String {
    if currently_checked {
        let reopened = set_frontmatter_scalar(body, "status", Some("open"));
        return set_frontmatter_scalar(&reopened, "completedDate", None);
    }
    let done = set_frontmatter_scalar(body, "status", Some("done"));
    set_frontmatter_scalar(&done, "completedDate", Some(&today_iso_local()))
}

/* ---------- Vault scan ------------------------------------------------ */

/// `normalizeTasksExcludedFolder` (also the included-folder rule): forward
/// slashes, no empty or dot segments, no traversal, at most 512 UTF-16 units.
fn normalize_tasks_folder(value: &serde_json::Value) -> Option<String> {
    let raw = value.as_str()?;
    let posix = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for seg in posix.split('/') {
        let s = js_trim(seg);
        if s.is_empty() {
            continue;
        }
        if s == "." || s == ".." {
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

/// `normalizeTasksExcludedFolders`: invalid entries drop, duplicates
/// collapse, order is kept; anything but an array is `[]`.
fn normalize_tasks_folders(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(entries) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for cleaned in entries.iter().filter_map(normalize_tasks_folder) {
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    out
}

fn is_inside_any_folder(rel: &str, folders: &[String]) -> bool {
    folders
        .iter()
        .any(|f| rel == f || rel.strip_prefix(f.as_str()).is_some_and(|r| r.starts_with('/')))
}

/// `isPathExcludedFromTasks`: segment-prefix, case-sensitive. A non-empty
/// `included` list (#962) leaves out everything outside it; `excluded`
/// (#458) still applies inside it.
fn is_path_excluded_from_tasks(rel: &str, excluded: &[String], included: &[String]) -> bool {
    if !included.is_empty() && !is_inside_any_folder(rel, included) {
        return true;
    }
    is_inside_any_folder(rel, excluded)
}

/// `readTasksFolderLists`: `tasks.excludedFolders` / `tasks.includedFolders`
/// off `.zennotes/vault.json`; a missing or malformed file means no lists.
fn read_tasks_folder_lists(root: &Path) -> (Vec<String>, Vec<String>) {
    let settings = root.join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE);
    let Ok(bytes) = fs::read(settings) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(raw) = serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&bytes))
    else {
        return (Vec::new(), Vec::new());
    };
    match raw.get("tasks").filter(|t| t.is_object() || t.is_array()) {
        Some(tasks) => (
            normalize_tasks_folders(tasks.get("excludedFolders")),
            normalize_tasks_folders(tasks.get("includedFolders")),
        ),
        None => (Vec::new(), Vec::new()),
    }
}

/// The note text as Node's `readFile(…, 'utf8')` hands it over: invalid UTF-8
/// becomes U+FFFD rather than an error, and a BOM stays.
fn read_note_text(abs: &Path) -> std::io::Result<String> {
    fs::read(abs).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// `scanAllTasks`: every task in the live notes (trash is skipped), honouring
/// the vault's folder lists and each note's `tasks:` switch unless
/// `include_excluded`. Upstream reads notes concurrently and pushes in
/// completion order; here notes come in `ops::list_note_files` order (the walk
/// under `list_notes`), each note's file task before its inline tasks.
pub fn scan_all_tasks(root: &Path, include_excluded: bool) -> Vec<VaultTask> {
    // The escape hatch scans past both lists: "everything" means every note.
    let (excluded, included) = if include_excluded {
        (Vec::new(), Vec::new())
    } else {
        read_tasks_folder_lists(root)
    };
    let mut out = Vec::new();
    for (abs, folder) in super::ops::list_note_files(root) {
        let rel = rel_of(root, &abs);
        if folder == "trash" || is_path_excluded_from_tasks(&rel, &excluded, &included) {
            continue;
        }
        let Ok(body) = read_note_text(&abs) else { continue };
        let title = title_from_path(&abs);
        let ctx = TaskContext { path: &rel, title: &title, folder: &folder };
        // File task first, then any inline checkboxes acting as subtasks.
        if let Some(file_task) = parse_task_file(&body, &ctx, include_excluded) {
            out.push(file_task);
        }
        out.extend(parse_tasks_from_body(&body, &ctx, include_excluded));
    }
    out
}

/* ---------- Toggle ---------------------------------------------------- */

/// `splitTaskId`: `"<path>#<index>"` split at the LAST `#`, so note names may
/// carry `#`. The index half is the literal `task` for a file task.
pub fn split_task_id(task_id: &str) -> Result<(String, String), String> {
    match task_id.rfind('#') {
        Some(i) => Ok((task_id[..i].to_string(), task_id[i + 1..].to_string())),
        None => Err(format!("Malformed task id: {task_id}")),
    }
}

/// `parseTaskIndex`: `Number.parseInt(indexStr, 10)` accepted when it is a
/// non-negative integer. parseInt skips leading whitespace, takes an optional
/// sign and the leading digits, and ignores whatever follows (`3abc` is 3);
/// `-0` is 0; digits past f64 range are Infinity, hence malformed. An index
/// past `usize` cannot name a task, so it saturates and finds none.
fn parse_task_index(task_id: &str, index_str: &str) -> Result<usize, String> {
    let malformed = || format!("Malformed task index in id: {task_id}");
    let s = index_str.trim_start_matches(is_js_ws);
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits_len == 0 {
        return Err(malformed());
    }
    let value: f64 = s[..digits_len].parse().map_err(|_| malformed())?;
    if !value.is_finite() || (negative && value != 0.0) {
        return Err(malformed());
    }
    Ok(if value >= usize::MAX as f64 { usize::MAX } else { value as usize })
}

/// `toggleTaskInBody`: flip the nth checkbox (fenced code skipped), or `None`
/// when the body has no task at that index. Open and done flip, `[/]` checks
/// off to done, and the `[>]` / `[-]` record markers are left alone (#599) —
/// the body is still returned, so the caller still rewrites it.
fn toggle_task_in_body(body: &str, target_index: usize) -> Option<String> {
    let normalized = body.replace("\r\n", "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
    let mut fence = FenceState { marker: None };
    let mut task_index = 0usize;
    let mut line_number = None;
    for (i, line) in lines.iter().enumerate() {
        if fence.skip(line) {
            continue;
        }
        if !TASK_LINE_RE.is_match(line).unwrap_or(false) {
            continue;
        }
        if task_index == target_index {
            line_number = Some(i);
            break;
        }
        task_index += 1;
    }
    let line_number = line_number?;
    let original = &lines[line_number];
    let state = captures(&TASK_LINE_RE, original).map(|c| group(&c, 1).to_string())?;
    if state != ">" && state != "-" {
        let next = if state == "x" || state == "X" { ' ' } else { 'x' };
        // Splice only the character inside the brackets, so the prefix (list
        // marker, quote, indentation) survives byte for byte. The prefix holds
        // no `[`, so the first `[<state>]` is the checkbox.
        if let Some(bracket) = original.find(&format!("[{state}]")) {
            let toggled = format!("{}{next}{}", &original[..=bracket], &original[bracket + 2..]);
            lines[line_number] = toggled;
        }
    }
    // Upstream appends a `\n` when `body` ends with one and the normalized
    // text does not, which cannot happen; the join is the whole result.
    Some(lines.join("\n"))
}

/// Node's message for a failed `fs.readFile`, which upstream lets through to
/// the agent as the tool error.
fn node_read_error(error: &std::io::Error, abs: &Path) -> String {
    let p = abs.display();
    match error.raw_os_error() {
        Some(2) => format!("ENOENT: no such file or directory, open '{p}'"),
        Some(13) => format!("EACCES: permission denied, open '{p}'"),
        Some(20) => format!("ENOTDIR: not a directory, open '{p}'"),
        Some(21) => "EISDIR: illegal operation on a directory, read".to_string(),
        _ => error.to_string(),
    }
}

/// `toggleTask`: flip the task `"<path>#<index>"` names and return it as it
/// now reads, or `None` when the id no longer matches a task. A `#task` id
/// flips a file task's frontmatter `status` (and `completedDate`). Explicit
/// ids are exclusion-blind: they only circulate via `list_tasks`, including
/// its includeExcluded listing. Like upstream, a toggle writes the note back
/// `\n`-normalized.
pub fn toggle_task(root: &Path, task_id: &str) -> Result<Option<VaultTask>, String> {
    let (rel, index_str) = split_task_id(task_id)?;

    if index_str == "task" {
        let abs = resolve_safe(root, &rel)?;
        let body = read_note_text(&abs).map_err(|e| node_read_error(&e, &abs))?;
        let note_rel = rel_of(root, &abs);
        let folder = super::ops::folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
        let title = title_from_path(&abs);
        let ctx = TaskContext { path: &note_rel, title: &title, folder: &folder };
        let Some(current) = parse_task_file(&body, &ctx, true) else {
            return Ok(None);
        };
        let next = toggle_file_task_in_body(&body, current.checked);
        prepare_note_creation(root, &note_rel)?;
        write_file_atomic(&abs, &next)?;
        return Ok(parse_task_file(&next, &ctx, true));
    }

    let target_index = parse_task_index(task_id, &index_str)?;
    let abs = resolve_safe(root, &rel)?;
    let body = read_note_text(&abs).map_err(|e| node_read_error(&e, &abs))?;
    let Some(new_body) = toggle_task_in_body(&body, target_index) else {
        return Ok(None);
    };
    let note_rel = rel_of(root, &abs);
    prepare_note_creation(root, &note_rel)?;
    write_file_atomic(&abs, &new_body)?;
    // Upstream checks the folder only after the write; the order is kept so a
    // stray id fails the same way (written, then reported).
    let folder = super::ops::folder_of(root, &abs).ok_or_else(|| format!("Note not in a known folder: {rel}"))?;
    let title = title_from_path(&abs);
    let ctx = TaskContext { path: &note_rel, title: &title, folder: &folder };
    // The toggle already landed; the re-parse must see past a `tasks:` opt-out.
    Ok(parse_tasks_from_body(&new_body, &ctx, true).into_iter().nth(target_index))
}

// Expected values below are what upstream's vault-ops.ts produced for the same
// input (run under bun against the upstream sources), not hand-derived.
#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(path: &'a str, title: &'a str, folder: &'a str) -> TaskContext<'a> {
        TaskContext { path, title, folder }
    }

    fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let abs = dir.path().join(rel);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        dir
    }

    fn ids(tasks: &[VaultTask]) -> Vec<String> {
        let mut out: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        out.sort();
        out
    }

    fn read(dir: &tempfile::TempDir, rel: &str) -> String {
        fs::read_to_string(dir.path().join(rel)).unwrap()
    }

    #[test]
    fn checkbox_states_list_markers_and_fences() {
        let body = [
            "# States", "- [ ] open", "- [x] done lower", "- [X] done upper", "- [>] forwarded",
            "- [/] in progress", "- [-] cancelled", "1. [ ] ordered dot", "10) [x] ordered paren",
            "> - [ ] quoted", "> > * [/] double quoted star", "+ [ ] plus", "\t  - [ ] tab indented",
            "-[ ] not a task", "a. [ ] not a task", "- [ ]", "- [ ]   ", "- [x]x glued",
            "- [?] unknown state", "```", "- [ ] in backtick fence", "~~~",
            "- [ ] still fenced (tilde does not close backticks)", "```", "    ~~~~",
            "- [ ] in tilde fence", "~~~", "- [ ] fenced: `~~~~` does not close `~~~`", "~~~~",
            "- [ ] still fenced", "",
        ]
        .join("\n");
        let tasks = parse_tasks_from_body(&body, &ctx("inbox/States.md", "States", "inbox"), false);
        let got: Vec<(usize, &str, &str, [bool; 4])> = tasks
            .iter()
            .map(|t| {
                let flags = [
                    t.checked,
                    t.cancelled.unwrap(),
                    t.in_progress.unwrap(),
                    t.forwarded.unwrap(),
                ];
                (t.line_number, t.raw_text.as_str(), t.content.as_str(), flags)
            })
            .collect();
        let n = [false; 4];
        assert_eq!(
            got,
            vec![
                (1, "- [ ] open", "open", n),
                (2, "- [x] done lower", "done lower", [true, false, false, false]),
                (3, "- [X] done upper", "done upper", [true, false, false, false]),
                (4, "- [>] forwarded", "forwarded", [false, false, false, true]),
                (5, "- [/] in progress", "in progress", [false, false, true, false]),
                (6, "- [-] cancelled", "cancelled", [false, true, false, false]),
                (7, "1. [ ] ordered dot", "ordered dot", n),
                (8, "10) [x] ordered paren", "ordered paren", [true, false, false, false]),
                (9, "> - [ ] quoted", "quoted", n),
                (10, "> > * [/] double quoted star", "double quoted star", [false, false, true, false]),
                (11, "+ [ ] plus", "plus", n),
                (12, "\t  - [ ] tab indented", "tab indented", n),
                (15, "- [ ]", "", n),
                (16, "- [ ]   ", "", n),
                (17, "- [x]x glued", "x glued", [true, false, false, false]),
                (25, "- [ ] in tilde fence", "in tilde fence", n),
            ]
        );
        // Exact upstream JSON, key order included.
        assert_eq!(
            serde_json::to_string(&tasks[3]).unwrap(),
            r#"{"id":"inbox/States.md#3","sourcePath":"inbox/States.md","link":"zennotes-rs://open?path=inbox/States.md","noteTitle":"States","noteFolder":"inbox","lineNumber":4,"taskIndex":3,"rawText":"- [>] forwarded","content":"forwarded","checked":false,"cancelled":false,"inProgress":false,"forwarded":true,"waiting":false,"tags":[]}"#
        );
    }

    #[test]
    fn inline_tokens() {
        let lines = [
            "- [ ] a due:2026-01-05 b",
            "- [ ] spaced due:   2026-02-03 x",
            "- [ ] upper DUE:2026-13-01 bad",
            "- [ ] feb due:2026-02-30",
            "- [ ] two due:2026-01-01 due:2026-01-02",
            "- [ ] pri !high one",
            "- [ ] pri !m two",
            "- [ ] pri !medium three",
            "- [ ] pri !hx none",
            "- [ ] pri !H upper",
            "- [ ] pri !l. dot",
            "- [ ] pri !lowx none",
            "- [ ] pri x!high glued",
            "- [ ] wait @waiting now",
            "- [ ] wait @Waiting!",
            "- [ ] wait @waitingx no",
            "- [ ] st @status:Review",
            "- [ ] st @STATUS:Doing",
            "- [ ] st @sprint:24 @status:qa",
            "- [ ] st @status:",
            "- [ ] tags #Tag #tag #Ünïcode #日本語 #a/b-c_d #123 x#no #-bad",
            "- [ ] due:2026-01-01 !h @waiting",
            "- [ ] spaces\u{a0}and\u{feff}bom\u{85}nel   tabs\t\tend  ",
        ];
        let tasks = parse_tasks_from_body(&lines.join("\n"), &ctx("inbox/Inline.md", "Inline", "inbox"), false);
        type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>, bool, Option<&'a str>, Vec<&'a str>);
        let got: Vec<Row> = tasks
            .iter()
            .map(|t| {
                (
                    t.content.as_str(),
                    t.due.as_deref(),
                    t.priority.as_deref(),
                    t.waiting,
                    t.status.as_deref(),
                    t.tags.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        let none = Vec::new;
        assert_eq!(
            got,
            vec![
                ("a b", Some("2026-01-05"), None, false, None, none()),
                ("spaced x", Some("2026-02-03"), None, false, None, none()),
                ("upper bad", None, None, false, None, none()),
                ("feb", Some("2026-02-30"), None, false, None, none()),
                ("two due:2026-01-02", Some("2026-01-01"), None, false, None, none()),
                ("pri one", None, Some("high"), false, None, none()),
                ("pri two", None, Some("med"), false, None, none()),
                ("pri three", None, Some("med"), false, None, none()),
                ("pri !hx none", None, None, false, None, none()),
                ("pri upper", None, Some("high"), false, None, none()),
                ("pri . dot", None, Some("low"), false, None, none()),
                ("pri !lowx none", None, None, false, None, none()),
                ("pri x!high glued", None, None, false, None, none()),
                ("wait now", None, None, true, None, none()),
                ("wait !", None, None, true, None, none()),
                ("wait @waitingx no", None, None, false, None, none()),
                ("st @status:Review", None, None, false, Some("review"), none()),
                ("st @STATUS:Doing", None, None, false, Some("doing"), none()),
                ("st @sprint:24 @status:qa", None, None, false, Some("qa"), none()),
                ("st @status:", None, None, false, None, none()),
                (
                    "tags #Tag #tag #Ünïcode #日本語 #a/b-c_d #123 x#no #-bad",
                    None,
                    None,
                    false,
                    None,
                    vec!["tag", "ünïcode", "日本語", "a/b-c_d", "123"]
                ),
                // Every token stripped: content falls back to the whole tail.
                ("due:2026-01-01 !h @waiting", Some("2026-01-01"), Some("high"), true, None, none()),
                // JS whitespace: NBSP and BOM collapse, NEL (U+0085) does not.
                ("spaces and bom\u{85}nel tabs end", None, None, false, None, none()),
            ]
        );
    }

    #[test]
    fn javascript_whitespace_and_line_terminators() {
        let body = [
            "   ```",
            "- [ ] fenced by 3-space-indented fence",
            "   ```",
            "\u{a0}- [ ] nbsp indented",
            "\u{3000}-\u{2003}[ ] ideographic and em space",
            "\u{85}- [ ] nel indented",
            "- [ ] lone\rcr",
            "- [ ] line\u{2028}sep",
            "\u{feff}- [ ] bom indented",
        ]
        .join("\n");
        let tasks = parse_tasks_from_body(&body, &ctx("inbox/U.md", "U", "inbox"), false);
        let got: Vec<&str> = tasks.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(got, vec!["nbsp indented", "ideographic and em space", "bom indented"]);
    }

    #[test]
    fn note_defaults_and_tasks_switch() {
        let defaults = "---\ndue: \"2026-03-03\"\npriority: 'Low'\nstatus: Blocked\n# comment: ignored\n:nokey\n---\n- [ ] inherits\n- [ ] own due:2026-04-04 !high @status:Done\n";
        let c = ctx("inbox/Defaults.md", "Defaults", "inbox");
        let tasks = parse_tasks_from_body(defaults, &c, false);
        assert_eq!(
            (tasks[0].due.as_deref(), tasks[0].priority.as_deref(), tasks[0].status.as_deref()),
            (Some("2026-03-03"), Some("low"), Some("blocked"))
        );
        assert_eq!(
            serde_json::to_string(&tasks[1]).unwrap(),
            r#"{"id":"inbox/Defaults.md#1","sourcePath":"inbox/Defaults.md","link":"zennotes-rs://open?path=inbox/Defaults.md","noteTitle":"Defaults","noteFolder":"inbox","lineNumber":8,"taskIndex":1,"rawText":"- [ ] own due:2026-04-04 !high @status:Done","content":"own @status:Done","checked":false,"cancelled":false,"inProgress":false,"forwarded":false,"due":"2026-04-04","priority":"high","waiting":false,"status":"done","tags":[]}"#
        );

        // Empty status, unknown priority, malformed due: nothing inherited.
        let none = "---\nstatus: \"\"\npriority: urgent\ndue: 2026-1-1\nTasks: ALL\n---\n- [ ] nothing inherited\n";
        let t = &parse_tasks_from_body(none, &c, false)[0];
        assert_eq!((t.due.as_ref(), t.priority.as_ref(), t.status.as_ref()), (None, None, None));

        let opt_out = "---\ntags: [task]\ntasks: false\n---\n- [ ] hidden one\n- [x] hidden two\n";
        assert!(parse_tasks_from_body(opt_out, &c, false).is_empty());
        assert!(parse_task_file(opt_out, &c, false).is_none());
        assert_eq!(parse_tasks_from_body(opt_out, &c, true).len(), 2);
        assert!(parse_task_file(opt_out, &c, true).is_some());
        assert!(parse_tasks_from_body("---\ntasks: Off\n---\n- [ ] off\n", &c, false).is_empty());

        // `tasks: note` keeps the file task and drops the checklist.
        let note_only = "---\ntags: [task, Project]\ntasks: note\nstatus: todo\n---\n- [ ] checklist item\n";
        assert!(parse_tasks_from_body(note_only, &c, false).is_empty());
        let file = parse_task_file(note_only, &c, false).unwrap();
        assert_eq!((file.status.as_deref(), file.tags.clone()), (Some("todo"), vec!["project".to_string()]));
    }

    #[test]
    fn file_tasks_from_frontmatter() {
        let body = [
            "---", "title: \"  Ship it \"", "status: In-Progress", "priority: normal",
            "due: '2026-05-01'", "scheduled: 2026-04-20", "completedDate: 2026-04-21", "tags:",
            "  - task", "  - \"#Work\"", "  # comment inside list", "", "  - Ünï",
            "contexts: [ \"@home\" , ]", "---", "# Ship it", "- [ ] subtask one", "",
        ]
        .join("\n");
        let c = ctx("inbox/Projects/FileTask.md", "FileTask", "inbox");
        assert_eq!(
            serde_json::to_string(&parse_task_file(&body, &c, false).unwrap()).unwrap(),
            r#"{"id":"inbox/Projects/FileTask.md#task","sourcePath":"inbox/Projects/FileTask.md","link":"zennotes-rs://open?path=inbox/Projects/FileTask.md","noteTitle":"FileTask","noteFolder":"inbox","lineNumber":0,"taskIndex":-1,"rawText":"","content":"Ship it","checked":false,"cancelled":false,"inProgress":true,"due":"2026-05-01","priority":"med","waiting":false,"status":"in-progress","tags":["work","ünï"],"kind":"file","scheduled":"2026-04-20","completedDate":"2026-04-21"}"#
        );
        assert_eq!(
            serde_json::to_string(
                &parse_task_file(
                    "---\ntags: task\nstatus: waiting\npriority: \"H\"\ndue: 2026-1-1\ntitle:\n---\nbody",
                    &ctx("archive/Waiting.md", "Waiting", "archive"),
                    false
                )
                .unwrap()
            )
            .unwrap(),
            r#"{"id":"archive/Waiting.md#task","sourcePath":"archive/Waiting.md","link":"zennotes-rs://open?path=archive/Waiting.md","noteTitle":"Waiting","noteFolder":"archive","lineNumber":0,"taskIndex":-1,"rawText":"","content":"Waiting","checked":false,"cancelled":false,"inProgress":false,"priority":"high","waiting":true,"status":"waiting","tags":[],"kind":"file"}"#
        );
        let status = |body: &str| parse_task_file(body, &c, false).map(|t| (t.status, t.checked, t.cancelled, t.in_progress));
        assert_eq!(status("---\ntags: \"#task\"\nstatus: canceled\n---\n"), Some((Some("canceled".into()), false, Some(true), Some(false))));
        assert_eq!(status("---\ntags: [task]\nstatus:\n  - Doing\n---\n"), Some((Some("doing".into()), false, Some(false), Some(true))));
        assert_eq!(status("---\ntags: [task]\nstatus: ''\n---\n"), Some((Some(String::new()), false, Some(false), Some(false))));
        assert_eq!(status("---\r\ntags: [task, Home]\r\nstatus: Done\r\n---\r\n"), Some((Some("done".into()), true, Some(false), Some(false))));
        assert_eq!(status("---\ntags: [task]\n---\n"), Some((Some("open".into()), false, Some(false), Some(false))));
        // No closing fence, a BOM before `---`, or no `task` tag: not a file task.
        assert_eq!(status("---\ntags: [task]\n- [ ] not frontmatter\n"), None);
        assert_eq!(status("\u{feff}---\ntags: [task]\n---\n"), None);
        assert_eq!(status("---\ntags: [project]\n---\n"), None);
    }

    fn fixture_a() -> tempfile::TempDir {
        vault(&[
            (
                ".zennotes/vault.json",
                r#"{"primaryNotesLocation":"inbox","tasks":{"excludedFolders":["inbox/Excluded"," inbox//Odd ","bad/../x",7,"inbox\\Back","inbox/Excluded"]}}"#,
            ),
            ("inbox/Plain.md", "- [ ] plain\n"),
            ("inbox/Projects/FileTask.md", "---\ntags: [task]\n---\n- [ ] sub one\n- [x] sub two\n"),
            ("inbox/OptOut.md", "---\ntags: [task]\ntasks: false\n---\n- [ ] hidden\n"),
            ("inbox/Excluded/Hidden.md", "- [ ] excluded folder\n"),
            ("inbox/Excluded/sub/Deeper.md", "- [ ] excluded deeper\n"),
            ("inbox/Excluded2.md", "- [ ] sibling not excluded\n"),
            ("inbox/Odd/o.md", "- [ ] odd excluded\n"),
            ("inbox/Back/b.md", "- [ ] backslash excluded\n"),
            ("inbox/DB.base/rec.md", "- [ ] database record\n"),
            ("inbox/Draw.excalidraw.md", "- [ ] in excalidraw md\n"),
            ("inbox/drawing.excalidraw", "{}\n- [ ] not md\n"),
            ("inbox/C# notes.md", "- [ ] hash name\n"),
            ("inbox/.dotnote.md", "- [ ] dot note\n"),
            ("inbox/.hidden/h.md", "- [ ] hidden dir\n"),
            ("inbox/README.MD", "- [ ] upper ext\n"),
            ("quick/CRLF.md", "- [ ] one\r\n- [x] two\r\n"),
            ("quick/Done.md", "---\r\ntags: [task, Home]\r\nstatus: Done\r\ncompletedDate: 2026-01-02\r\n---\r\nbody\r\n- [ ] crlf sub\r\n"),
            ("archive/Waiting.md", "---\ntags: task\nstatus: waiting\n---\nbody"),
            ("trash/Trashed.md", "---\ntags: [task]\n---\n- [ ] trashed\n"),
            ("assets/t.md", "- [ ] asset task\n"),
            ("Loose.md", "- [ ] loose root note\n"),
        ])
    }

    #[test]
    fn scan_honours_folder_lists_and_skips_non_notes() {
        let dir = fixture_a();
        let scan = scan_all_tasks(dir.path(), false);
        assert_eq!(
            ids(&scan),
            [
                "archive/Waiting.md#task", "inbox/.dotnote.md#0", "inbox/C# notes.md#0",
                "inbox/Draw.excalidraw.md#0", "inbox/Excluded2.md#0", "inbox/Plain.md#0",
                "inbox/Projects/FileTask.md#0", "inbox/Projects/FileTask.md#1",
                "inbox/Projects/FileTask.md#task", "inbox/README.MD#0", "quick/CRLF.md#0",
                "quick/CRLF.md#1", "quick/Done.md#0", "quick/Done.md#task",
            ]
        );
        let all = scan_all_tasks(dir.path(), true);
        let extra: Vec<String> = ids(&all).into_iter().filter(|id| !ids(&scan).contains(id)).collect();
        assert_eq!(
            extra,
            [
                "inbox/Back/b.md#0", "inbox/Excluded/Hidden.md#0", "inbox/Excluded/sub/Deeper.md#0",
                "inbox/Odd/o.md#0", "inbox/OptOut.md#0", "inbox/OptOut.md#task",
            ]
        );
        // Deterministic: a note's file task comes right before its checkboxes.
        let pos = |id: &str| scan.iter().position(|t| t.id == id).unwrap();
        assert_eq!(pos("inbox/Projects/FileTask.md#0"), pos("inbox/Projects/FileTask.md#task") + 1);
        assert_eq!(pos("inbox/Projects/FileTask.md#1"), pos("inbox/Projects/FileTask.md#0") + 1);
        assert_eq!(scan_all_tasks(dir.path(), false), scan);

        let done = scan.iter().find(|t| t.id == "quick/Done.md#task").unwrap();
        assert_eq!(
            serde_json::to_string(done).unwrap(),
            r#"{"id":"quick/Done.md#task","sourcePath":"quick/Done.md","link":"zennotes-rs://open?path=quick/Done.md","noteTitle":"Done","noteFolder":"quick","lineNumber":0,"taskIndex":-1,"rawText":"","content":"Done","checked":true,"cancelled":false,"inProgress":false,"waiting":false,"status":"done","tags":["home"],"kind":"file","completedDate":"2026-01-02"}"#
        );
        let hash = scan.iter().find(|t| t.id == "inbox/C# notes.md#0").unwrap();
        assert_eq!((hash.link.as_str(), hash.note_title.as_str()), ("zennotes-rs://open?path=inbox/C%23%20notes.md", "C# notes"));
    }

    #[test]
    fn scan_with_included_folders() {
        let dir = vault(&[
            (
                ".zennotes/vault.json",
                r#"{"tasks":{"includedFolders":["inbox/Projects","quick",""],"excludedFolders":["inbox/Projects/Archive"]}}"#,
            ),
            ("inbox/Projects/p.md", "- [ ] in project\n"),
            ("inbox/Projects/Archive/old.md", "- [ ] archived project\n"),
            ("inbox/Other.md", "- [ ] outside included\n"),
            ("quick/q.md", "- [ ] quick included\n"),
            ("archive/a.md", "- [ ] archive outside\n"),
        ]);
        assert_eq!(ids(&scan_all_tasks(dir.path(), false)), ["inbox/Projects/p.md#0", "quick/q.md#0"]);
        assert_eq!(scan_all_tasks(dir.path(), true).len(), 5);
    }

    #[test]
    fn scan_and_toggle_in_root_mode() {
        let dir = vault(&[
            (".zennotes/vault.json", r#"{"primaryNotesLocation":"root","tasks":5}"#),
            ("Root.md", "- [ ] at root\n"),
            ("Folder/f.md", "---\ntags: [task]\n---\n- [ ] in folder\n"),
            ("quick/q.md", "- [x] quick\n"),
            ("assets/a.md", "- [ ] asset\n"),
            ("attachements/a.md", "- [ ] legacy attachments\n"),
            (".zennotes/n.md", "- [ ] internal\n"),
            ("inbox/i.md", "- [ ] literal inbox dir in root mode\n"),
        ]);
        let expected = ["Folder/f.md#0", "Folder/f.md#task", "Root.md#0", "inbox/i.md#0", "quick/q.md#0"];
        assert_eq!(ids(&scan_all_tasks(dir.path(), false)), expected);
        assert_eq!(ids(&scan_all_tasks(dir.path(), true)), expected);
        let folder = |id: &str| toggle_task(dir.path(), id).map(|t| t.unwrap().note_folder);
        assert_eq!(folder("Root.md#0"), Ok("inbox".into()));
        assert_eq!(folder("Folder/f.md#task"), Ok("inbox".into()));
        assert_eq!(folder("quick/q.md#0"), Ok("quick".into()));
        assert_eq!(folder("assets/a.md#0"), Err("Note not in a known folder: assets/a.md".into()));
        assert_eq!(read(&dir, "Root.md"), "- [x] at root\n");
        assert_eq!(read(&dir, "quick/q.md"), "- [ ] quick\n");
        // Like upstream, the folder check comes after the write.
        assert_eq!(read(&dir, "assets/a.md"), "- [x] asset\n");
    }

    #[test]
    fn toggle_inline_tasks() {
        let dir = vault(&[(
            "inbox/States.md",
            "# S\n- [ ] open\n- [x] done\n- [X] upper\n- [>] fwd\n- [/] wip\n- [-] gone\n> 1) [ ] quoted\n",
        )]);
        let root = dir.path();
        let flip = |id: &str| toggle_task(root, id).map(|t| t.map(|t| (t.id, t.raw_text)));
        let ok = |id: &str, raw: &str| Ok(Some((id.to_string(), raw.to_string())));
        assert_eq!(flip("inbox/States.md#0"), ok("inbox/States.md#0", "- [x] open"));
        assert_eq!(flip("inbox/States.md#1"), ok("inbox/States.md#1", "- [ ] done"));
        assert_eq!(flip("inbox/States.md#2"), ok("inbox/States.md#2", "- [ ] upper"));
        // Forwarded and cancelled records are left alone (#599).
        assert_eq!(flip("inbox/States.md#3"), ok("inbox/States.md#3", "- [>] fwd"));
        assert_eq!(flip("inbox/States.md#4"), ok("inbox/States.md#4", "- [x] wip"));
        assert_eq!(flip("inbox/States.md#5"), ok("inbox/States.md#5", "- [-] gone"));
        // parseInt rules: leading space, trailing junk, `-0`, `+n`; and the
        // returned id carries the normalized path.
        assert_eq!(flip("inbox/./States.md# 6abc"), ok("inbox/States.md#6", "> 1) [x] quoted"));
        assert_eq!(flip("inbox/States.md#-0"), ok("inbox/States.md#0", "- [ ] open"));
        assert_eq!(flip("inbox/States.md#+1"), ok("inbox/States.md#1", "- [x] done"));
        assert_eq!(flip("inbox/States.md#99"), Ok(None));
        assert_eq!(flip("inbox/States.md#999999999999999999999999"), Ok(None));
        assert_eq!(
            read(&dir, "inbox/States.md"),
            "# S\n- [ ] open\n- [x] done\n- [ ] upper\n- [>] fwd\n- [x] wip\n- [-] gone\n> 1) [x] quoted\n"
        );
    }

    #[test]
    fn toggle_file_tasks_rewrites_frontmatter() {
        let dir = fixture_a();
        let root = dir.path();
        let reopened = toggle_task(root, "quick/Done.md#task").unwrap().unwrap();
        assert_eq!(
            serde_json::to_string(&reopened).unwrap(),
            r#"{"id":"quick/Done.md#task","sourcePath":"quick/Done.md","link":"zennotes-rs://open?path=quick/Done.md","noteTitle":"Done","noteFolder":"quick","lineNumber":0,"taskIndex":-1,"rawText":"","content":"Done","checked":false,"cancelled":false,"inProgress":false,"waiting":false,"status":"open","tags":["home"],"kind":"file"}"#
        );
        // CRLF comes back LF-normalized, as upstream writes it.
        assert_eq!(read(&dir, "quick/Done.md"), "---\ntags: [task, Home]\nstatus: open\n---\nbody\n- [ ] crlf sub\n");

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let done = toggle_task(root, "archive/Waiting.md#task").unwrap().unwrap();
        assert_eq!((done.checked, done.completed_date.as_deref()), (true, Some(today.as_str())));
        assert_eq!(
            read(&dir, "archive/Waiting.md"),
            format!("---\ntags: task\nstatus: done\ncompletedDate: {today}\n---\nbody")
        );
        // Exclusion-blind: an explicit id toggles an opted-out note too.
        assert!(toggle_task(root, "inbox/OptOut.md#task").unwrap().unwrap().checked);
        assert_eq!(toggle_task(root, "inbox/OptOut.md#0").unwrap().unwrap().raw_text, "- [x] hidden");
        assert_eq!(toggle_task(root, "inbox/Plain.md#task"), Ok(None));
        assert_eq!(toggle_task(root, "inbox/C# notes.md#0").unwrap().unwrap().id, "inbox/C# notes.md#0");
        assert_eq!(toggle_task(root, "Loose.md#0").unwrap().unwrap().note_folder, "inbox");
        assert_eq!(read(&dir, "quick/CRLF.md"), "- [ ] one\r\n- [x] two\r\n");
        assert_eq!(toggle_task(root, "quick/CRLF.md#1").unwrap().unwrap().raw_text, "- [ ] two");
        assert_eq!(read(&dir, "quick/CRLF.md"), "- [ ] one\n- [ ] two\n");
    }

    #[test]
    fn toggle_errors_match_upstream_messages() {
        let dir = fixture_a();
        let root = dir.path();
        let err = |id: &str| toggle_task(root, id).unwrap_err().replace(&*root.to_string_lossy(), "<ROOT>");
        assert_eq!(err("nohash"), "Malformed task id: nohash");
        assert_eq!(err("inbox/Plain.md#abc"), "Malformed task index in id: inbox/Plain.md#abc");
        assert_eq!(err("inbox/Plain.md#-1"), "Malformed task index in id: inbox/Plain.md#-1");
        assert_eq!(err("inbox/Plain.md#"), "Malformed task index in id: inbox/Plain.md#");
        assert_eq!(err("../x.md#0"), "Path escapes vault: ../x.md");
        assert_eq!(err("inbox/Missing.md#0"), "ENOENT: no such file or directory, open '<ROOT>/inbox/Missing.md'");
        assert_eq!(err("inbox/Missing.md#task"), "ENOENT: no such file or directory, open '<ROOT>/inbox/Missing.md'");
        assert_eq!(err("inbox/Projects#0"), "EISDIR: illegal operation on a directory, read");
        assert_eq!(err("assets/t.md#task"), "Note not in a known folder: assets/t.md");
        assert_eq!(split_task_id("a#b#c"), Ok(("a#b".to_string(), "c".to_string())));
    }
}
