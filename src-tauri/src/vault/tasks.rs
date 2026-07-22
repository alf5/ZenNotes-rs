//! Task scanning — port of `parseTasksFromBody` (packages/shared-domain/
//! tasks.ts) plus the main-process `scanAllTasks` / `scanTasksForPath`
//! (apps/desktop/src/main/tasks.ts). Index counting matches the TS exactly so
//! task ids stay stable across content edits.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::ipc::types::VaultTask;
use crate::vault::listing::list_notes;
use crate::vault::notes::{folder_for_relative_path, title_from_path, to_posix};

const LIVE_FOLDERS: [&str; 3] = ["inbox", "quick", "archive"];

static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)(```|~~~)").unwrap());
static TASK_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\s*(?:>\s*)*(?:[-+*]|[0-9]+[.)])\s+\[)( |x|X)(\].*)$").unwrap()
});
static FRONTMATTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---\n([\s\S]*?)\n---\n?").unwrap());
static INLINE_DUE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|\s)due:(\S+)").unwrap());
static INLINE_PRIORITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|\s)!(high|med|medium|low|h|m|l)\b").unwrap());
static INLINE_WAITING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|\s)@waiting\b").unwrap());
static INLINE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|\s)#([a-z0-9][a-z0-9/_-]*)").unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

#[derive(Default)]
struct NoteDefaults {
    due: Option<String>,
    priority: Option<String>,
}

fn unquote(v: &str) -> String {
    let trimmed = v.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() >= 2 {
        let first = chars[0];
        let last = chars[chars.len() - 1];
        if (first == '"' || first == '\'') && first == last {
            return chars[1..chars.len() - 1].iter().collect();
        }
    }
    trimmed.to_string()
}

fn normalize_priority(raw: &str) -> Option<String> {
    match raw.to_lowercase().trim() {
        "high" | "h" => Some("high".to_string()),
        "med" | "medium" | "m" => Some("med".to_string()),
        "low" | "l" => Some("low".to_string()),
        _ => None,
    }
}

fn is_valid_iso_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
        && s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
}

fn normalize_due_date(raw: &str) -> Option<String> {
    let cleaned = unquote(raw.trim());
    if is_valid_iso_date(&cleaned) {
        Some(cleaned)
    } else {
        None
    }
}

fn parse_note_defaults(body: &str) -> NoteDefaults {
    let mut defaults = NoteDefaults::default();
    let Ok(Some(caps)) = FRONTMATTER_RE.captures(body) else {
        return defaults;
    };
    let block = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    for raw_line in block.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else { continue };
        if colon < 1 {
            continue;
        }
        let key = line[..colon].trim().to_lowercase();
        let value = unquote(&line[colon + 1..]);
        match key.as_str() {
            "due" => {
                if let Some(d) = normalize_due_date(&value) {
                    defaults.due = Some(d);
                }
            }
            "priority" => {
                if let Some(p) = normalize_priority(&value) {
                    defaults.priority = Some(p);
                }
            }
            _ => {}
        }
    }
    defaults
}

struct ExtractedTokens {
    due: Option<String>,
    priority: Option<String>,
    waiting: bool,
    tags: Vec<String>,
    stripped: String,
}

fn extract_tokens(tail: &str) -> ExtractedTokens {
    let mut due = None;
    let mut priority = None;
    let mut waiting = false;
    let mut tags: Vec<String> = Vec::new();
    let mut stripped = tail.to_string();

    if let Ok(Some(caps)) = INLINE_DUE_RE.captures(&stripped) {
        if let Some(candidate) = caps.get(1) {
            if is_valid_iso_date(candidate.as_str()) {
                due = Some(candidate.as_str().to_string());
            }
        }
        stripped = INLINE_DUE_RE.replace(&stripped, " ").into_owned();
    }

    if let Ok(Some(caps)) = INLINE_PRIORITY_RE.captures(&stripped) {
        if let Some(p) = caps.get(1) {
            priority = normalize_priority(p.as_str());
        }
        stripped = INLINE_PRIORITY_RE.replace(&stripped, " ").into_owned();
    }

    if INLINE_WAITING_RE.is_match(&stripped).unwrap_or(false) {
        waiting = true;
        stripped = INLINE_WAITING_RE.replace(&stripped, " ").into_owned();
    }

    // Tags are scanned over the ORIGINAL tail (matching the TS).
    for caps in INLINE_TAG_RE.captures_iter(tail).flatten() {
        if let Some(m) = caps.get(1) {
            let tag = m.as_str().to_lowercase();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    let stripped = WHITESPACE_RE.replace_all(&stripped, " ").trim().to_string();
    ExtractedTokens { due, priority, waiting, tags, stripped }
}

/// Parse every checkbox in `body`, skipping fenced code.
pub fn parse_tasks_from_body(body: &str, path: &str, title: &str, folder: &str) -> Vec<VaultTask> {
    let normalized = body.replace("\r\n", "\n");
    let defaults = parse_note_defaults(&normalized);
    let mut tasks = Vec::new();
    let mut task_index = 0i64;
    let mut in_fence = false;
    let mut fence_marker: Option<String> = None;

    for (i, line) in normalized.split('\n').enumerate() {
        if let Ok(Some(fence)) = FENCE_RE.captures(line) {
            let marker = fence.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            if !in_fence {
                in_fence = true;
                fence_marker = Some(marker);
            } else if Some(&marker) == fence_marker.as_ref() {
                in_fence = false;
                fence_marker = None;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        let Ok(Some(task)) = TASK_LINE_RE.captures(line) else { continue };
        let checked_char = task.get(2).map(|m| m.as_str()).unwrap_or(" ");
        let group3 = task.get(3).map(|m| m.as_str()).unwrap_or("");
        let tail = group3.strip_prefix(']').unwrap_or(group3);
        let checked = checked_char == "x" || checked_char == "X";
        let tokens = extract_tokens(tail);
        let content = if tokens.stripped.is_empty() {
            tail.trim().to_string()
        } else {
            tokens.stripped.clone()
        };

        tasks.push(VaultTask {
            id: format!("{path}#{task_index}"),
            source_path: path.to_string(),
            note_title: title.to_string(),
            note_folder: folder.to_string(),
            line_number: i as i64,
            task_index,
            raw_text: line.to_string(),
            content,
            checked,
            due: tokens.due.or_else(|| defaults.due.clone()),
            priority: tokens.priority.or_else(|| defaults.priority.clone()),
            waiting: tokens.waiting,
            tags: tokens.tags,
        });
        task_index += 1;
    }
    tasks
}

/// `vault:scan-tasks` — every task in every live (non-trash) note.
pub fn scan_all_tasks(root: &Path) -> Vec<VaultTask> {
    let mut out = Vec::new();
    for meta in list_notes(root) {
        if meta.folder == "trash" || crate::vault::notes::is_excalidraw_path(&meta.path) {
            continue;
        }
        let abs = root.join(meta.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Ok(body) = fs::read_to_string(&abs) else { continue };
        out.extend(parse_tasks_from_body(&body, &meta.path, &meta.title, &meta.folder));
    }
    out
}

/// `vault:scan-tasks-for` — rescan a single live note's tasks.
pub fn scan_tasks_for_path(root: &Path, rel_path: &str) -> Vec<VaultTask> {
    let posix = to_posix(rel_path);
    if crate::vault::notes::is_excalidraw_path(&posix) {
        return Vec::new();
    }
    let Some(folder) = folder_for_relative_path(&posix) else {
        return Vec::new();
    };
    if !LIVE_FOLDERS.contains(&folder.as_str()) {
        return Vec::new();
    }
    let abs = root.join(posix.replace('/', std::path::MAIN_SEPARATOR_STR));
    let Ok(body) = fs::read_to_string(&abs) else {
        return Vec::new();
    };
    let title = title_from_path(Path::new(&posix));
    parse_tasks_from_body(&body, &posix, &title, &folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checkboxes_and_tokens() {
        let body = "# Notes\n- [ ] plain task\n- [x] done one !high due:2026-01-02 @waiting #work\n```\n- [ ] in code\n```\n1. [ ] ordered";
        let tasks = parse_tasks_from_body(body, "inbox/N.md", "N", "inbox");
        assert_eq!(tasks.len(), 3); // code-fenced checkbox excluded
        assert_eq!(tasks[0].content, "plain task");
        assert!(!tasks[0].checked);
        let t = &tasks[1];
        assert!(t.checked);
        assert_eq!(t.due.as_deref(), Some("2026-01-02"));
        assert_eq!(t.priority.as_deref(), Some("high"));
        assert!(t.waiting);
        assert_eq!(t.tags, vec!["work".to_string()]);
        // Tags are collected but NOT stripped from content (matches the TS).
        assert_eq!(t.content, "done one #work");
        assert_eq!(t.id, "inbox/N.md#1");
        assert_eq!(tasks[2].task_index, 2);
    }

    #[test]
    fn frontmatter_defaults_apply() {
        let body = "---\ndue: 2026-03-04\npriority: low\n---\n- [ ] inherit me";
        let tasks = parse_tasks_from_body(body, "inbox/D.md", "D", "inbox");
        assert_eq!(tasks[0].due.as_deref(), Some("2026-03-04"));
        assert_eq!(tasks[0].priority.as_deref(), Some("low"));
    }

    #[test]
    fn invalid_due_is_dropped() {
        let body = "- [ ] bad due:2026-13-40";
        let tasks = parse_tasks_from_body(body, "inbox/X.md", "X", "inbox");
        assert_eq!(tasks[0].due, None);
    }
}
