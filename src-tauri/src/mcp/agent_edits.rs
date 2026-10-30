//! Keeping what a note said before an assistant changed it, so the user can
//! review the change in the app (Settings → MCP → Review agent edits). Ported
//! from upstream apps/desktop/src/mcp/agent-edit-recorder.ts plus the writing
//! half of shared-domain's agent-edits.ts (`beginAgentEdit`,
//! `finishAgentEdit`); the reviewing half stays in the webview
//! (src/bridge/agent-edits.ts), which reads these same records.
//!
//! Every tool that rewrites a note's text runs through `run_recording`: the
//! record is written BEFORE the edit (so a process that dies mid-edit still
//! leaves the bytes to compare against), the tool runs, and the record then
//! notes how the note looks now. The writing side stays string comparisons
//! and JSON; every merge happens in the app.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ops, tasks};
use crate::vault::sidecars::{read_agent_edit_text, write_agent_edit_text};

const AGENT_EDIT_RECORD_VERSION: u64 = 1;
/// Notes larger than this are not recorded: a record holds several copies.
const MAX_AGENT_EDIT_NOTE_BYTES: usize = 1024 * 1024;
/// How many edits a record lists; older entries drop off, the texts do not.
const MAX_AGENT_EDIT_ENTRIES: usize = 50;
/// How many outside changes a record keeps apart before starting afresh.
const MAX_AGENT_EDIT_OUTSIDE: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: i64,
    pub agent: String,
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutsideChange {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    pub version: u64,
    pub before: String,
    pub after: Option<String>,
    pub edits: Vec<Entry>,
    pub outside: Vec<OutsideChange>,
}

/// The note a recorded tool rewrites. Creating, moving, renaming and deleting
/// notes are not recorded: a new note has nothing to compare against, and
/// Trash already keeps the rest.
fn recorded_note(tool: &str, args: &Map<String, Value>) -> Option<String> {
    let arg = |key: &str| args.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    match tool {
        "write_note" | "append_to_note" | "prepend_to_note" | "insert_at_line" | "replace_in_note" => {
            arg("path").map(str::to_string)
        }
        "toggle_task" => tasks::split_task_id(arg("id")?).ok().map(|(rel, _)| rel),
        _ => None,
    }
}

/// Settings → MCP → "Review agent edits" (`[mcp] review_agent_edits` in the
/// portable config). On when the file or the key is missing, the app's
/// default; off stops recording, and records already written stay.
fn review_agent_edits_enabled() -> bool {
    super::portable_pref("mcp", "review_agent_edits").unwrap_or(true)
}

/// Port of `parseAgentEditRecord`: null when missing or not a record.
fn parse_record(raw: &str) -> Option<Record> {
    let value: Value = serde_json::from_str(raw).ok()?;
    if value.get("version")?.as_u64()? != AGENT_EDIT_RECORD_VERSION {
        return None;
    }
    let before = value.get("before")?.as_str()?.to_string();
    let after = match value.get("after") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return None,
    };
    let list = |key: &str| value.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    let clip = |v: Option<&Value>| -> String {
        v.and_then(Value::as_str).map(|s| s.trim().chars().take(80).collect()).unwrap_or_default()
    };
    let edits = list("edits")
        .iter()
        .filter_map(|e| {
            let at = e.get("at")?.as_f64().filter(|f| f.is_finite())? as i64;
            let agent = clip(e.get("agent"));
            Some(Entry { at, agent: if agent.is_empty() { "Assistant".into() } else { agent }, tool: clip(e.get("tool")) })
        })
        .collect();
    let outside = list("outside")
        .iter()
        .filter_map(|c| {
            Some(OutsideChange { from: c.get("from")?.as_str()?.into(), to: c.get("to")?.as_str()?.into() })
        })
        .collect();
    Some(Record { version: AGENT_EDIT_RECORD_VERSION, before, after, edits, outside })
}

/// The record to write BEFORE an edit, given what is on disk and the note now.
fn begin(existing: Option<&Record>, current: &str) -> Record {
    let fresh = Record {
        version: AGENT_EDIT_RECORD_VERSION,
        before: current.to_string(),
        after: None,
        edits: Vec::new(),
        outside: Vec::new(),
    };
    let Some(existing) = existing else { return fresh };
    match &existing.after {
        None => Record { after: None, ..existing.clone() },
        Some(after) if after == current => Record { after: None, ..existing.clone() },
        Some(_) if existing.outside.len() >= MAX_AGENT_EDIT_OUTSIDE => fresh,
        Some(after) => {
            let mut outside = existing.outside.clone();
            outside.push(OutsideChange { from: after.clone(), to: current.to_string() });
            Record { after: None, outside, ..existing.clone() }
        }
    }
}

/// The record to write AFTER an edit.
fn finish(started: &Record, current: &str, entry: Entry) -> Record {
    let mut edits = started.edits.clone();
    edits.push(entry);
    let skip = edits.len().saturating_sub(MAX_AGENT_EDIT_ENTRIES);
    Record { after: Some(current.to_string()), edits: edits.split_off(skip), ..started.clone() }
}

fn write_record(root: &Path, rel: &str, record: Option<&Record>) -> Result<(), String> {
    let text = record.map(|r| format!("{}\n", serde_json::to_string(r).unwrap_or_default()));
    write_agent_edit_text(root, rel, text.as_deref())
}

fn note_text(root: &Path, rel: &str) -> Option<String> {
    ops::read_note(root, rel).ok().map(|n| n.body)
}

/// Run a tool, recording its edit when it rewrites a note. Recording never
/// stands in the way of the edit the user asked for: if the record cannot be
/// written, the tool still runs and the change is simply not offered for
/// review. Callers serialize tool calls, so two edits never race on a record.
pub fn run_recording<T>(
    tool: &str,
    args: &Map<String, Value>,
    root: &Path,
    agent: &str,
    run: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let Some(rel) = recorded_note(tool, args) else { return run() };
    if !review_agent_edits_enabled() {
        return run();
    }
    // Not a note we can read (the tool reports its own error), or one too
    // large to keep copies of.
    let Some(before) = note_text(root, &rel).filter(|t| t.len() <= MAX_AGENT_EDIT_NOTE_BYTES) else {
        return run();
    };
    let existing = read_agent_edit_text(root, &rel).as_deref().and_then(parse_record);
    let started = begin(existing.as_ref(), &before);
    if let Err(error) = write_record(root, &rel, Some(&started)) {
        eprintln!("zennotes mcp: could not record the edit to {rel}: {error}");
        return run();
    }
    let result = run();
    let after = note_text(root, &rel);
    let finished = match after {
        // Nothing changed (no match, a task that moved, a failed write): the
        // record goes back to what it was.
        None => write_record(root, &rel, existing.as_ref()),
        Some(text) if text == before => write_record(root, &rel, existing.as_ref()),
        Some(text) => {
            let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
            let entry = Entry { at, agent: agent.to_string(), tool: tool.to_string() };
            write_record(root, &rel, Some(&finish(&started, &text, entry)))
        }
    };
    if let Err(error) = finished {
        eprintln!("zennotes mcp: could not finish the record for {rel}: {error}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_folds_outside_changes() {
        let entry = Entry { at: 1, agent: "A".into(), tool: "write_note".into() };
        let first = finish(&begin(None, "v1"), "v2", entry.clone());
        assert_eq!(first.before, "v1");
        assert_eq!(first.after.as_deref(), Some("v2"));
        // The user typed between two assistant edits.
        let second = begin(Some(&first), "v2 + user");
        assert_eq!(second.after, None);
        assert_eq!(second.outside, vec![OutsideChange { from: "v2".into(), to: "v2 + user".into() }]);
        // No outside change: the record just reopens.
        assert_eq!(begin(Some(&first), "v2").outside, vec![]);
        let text = serde_json::to_string(&first).unwrap();
        assert_eq!(
            text,
            r#"{"version":1,"before":"v1","after":"v2","edits":[{"at":1,"agent":"A","tool":"write_note"}],"outside":[]}"#
        );
        assert_eq!(parse_record(&text), Some(first));
    }
}
