//! Tools for the jobs an assistant is asked to do most and could only do
//! badly before: changing a task without hand-editing its line, finding the
//! daily note without guessing a path, and working on part of a long note
//! without reading or rewriting all of it. Ported from upstream
//! apps/desktop/src/mcp/work-tools.ts. Each is composed from the backend's
//! own reads and writes, so a scope applies to them as to any other tool; the
//! text transforms live in task_edits.rs, periodic.rs and sections.rs.

use std::sync::LazyLock;

use chrono::{Local, NaiveDate, NaiveDateTime};
use fancy_regex::Regex;
use serde_json::{json, Value};

use super::backend::Backend;
use super::ops::{self, re};
use super::periodic::{self, NoteRef, PeriodicKind};
use super::scope::{go_quote, is_js_ws, js_trim};
use super::sections::{edit_section_in_body, find_section, note_sections, section_body, uses_crlf, SectionEditMode};
use super::server::{optional_boolean, optional_number, optional_string, require_string, Args};
use super::task_edits as edits;
use super::tasks::{split_task_id, today_iso_local, VaultTask};

/* ---------- update_task ------------------------------------------------ */

/// `YYYY-MM-DD` that names a real day.
fn is_valid_iso_date(value: &str) -> bool {
    let b = value.as_bytes();
    let shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    shape && NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

struct UpdateTaskInput<'a> {
    id: &'a str,
    due: Option<&'a str>,
    priority: Option<&'a str>,
    waiting: Option<bool>,
    state: Option<&'a str>,
    status: Option<&'a str>,
    text: Option<&'a str>,
    expected_text: Option<&'a str>,
}

/// What the task parser reads as an `@key:value` value.
static TASK_FIELD_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^[\p{L}0-9][\p{L}0-9/_-]*$"));

fn validate_update_task(input: &UpdateTaskInput) -> Result<(), String> {
    if input.due.is_none()
        && input.priority.is_none()
        && input.waiting.is_none()
        && input.state.is_none()
        && input.status.is_none()
        && input.text.is_none()
    {
        return Err("update_task needs at least one of due, priority, waiting, state, status, text".into());
    }
    if input.due.is_some_and(|d| !d.is_empty() && !is_valid_iso_date(d)) {
        return Err("due must be YYYY-MM-DD, or empty to clear it".into());
    }
    if input.priority.is_some_and(|p| !["", "high", "med", "low"].contains(&p)) {
        return Err("priority must be high, med or low, or empty to clear it".into());
    }
    if input.state.is_some_and(|s| !["open", "done", "in-progress", "cancelled"].contains(&s)) {
        return Err("state must be open, done, in-progress or cancelled".into());
    }
    if input.status.is_some_and(|s| !s.is_empty() && !TASK_FIELD_VALUE_RE.is_match(s).unwrap_or(false)) {
        return Err("status must be one word (letters, digits, /, _ or -), or empty to clear it".into());
    }
    if input.text.is_some_and(|t| js_trim(t).is_empty() || t.contains(['\r', '\n'])) {
        return Err("text must be one line, and not empty".into());
    }
    Ok(())
}

fn find_task(backend: &dyn Backend, rel: &str, index: &str) -> Result<Option<VaultTask>, String> {
    let suffix = format!("#{index}");
    Ok(backend.scan_tasks_for_path(rel, true)?.into_iter().find(|t| t.id.ends_with(&suffix)))
}

/// The edit is applied to the task's line on its own: an id is a position
/// in the note, so an edit since the scan can put another task there, and
/// every other line keeps its bytes, line ending included. The second value
/// is how far the task moved: finishing a recurring one writes its next
/// occurrence above it, as toggle_task does.
fn update_task_line(body: &str, task: &VaultTask, input: &UpdateTaskInput) -> Result<Option<(String, usize)>, String> {
    if task.forwarded == Some(true) && input.state.is_some() {
        return Err("That task was forwarded to another note; change it there.".into());
    }
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    let at = task.line_number;
    let tidy = |line: &str| line.trim_end_matches(is_js_ws).to_string();
    if at >= lines.len() || tidy(&lines[at]) != tidy(&task.raw_text) {
        return Ok(None);
    }
    let ending = if lines[at].ends_with('\r') { "\r" } else { "" };
    let mut line = lines[at][..lines[at].len() - ending.len()].to_string();
    if let Some(text) = input.text {
        line = edits::set_task_text_at_index(&line, 0, text);
    }
    if let Some(due) = input.due {
        line = edits::set_task_due_at_index(&line, 0, Some(due).filter(|d| !d.is_empty()));
    }
    if let Some(priority) = input.priority {
        line = edits::set_task_priority_at_index(&line, 0, Some(priority).filter(|p| !p.is_empty()));
    }
    if let Some(waiting) = input.waiting {
        line = edits::set_task_waiting_at_index(&line, 0, waiting);
    }
    if let Some(status) = input.status {
        line = edits::set_task_field_at_index(&line, 0, "status", Some(status).filter(|s| !s.is_empty()));
    }
    if input.state == Some("done") {
        lines[at] = format!("{line}{ending}");
        if let Some(recurring) = edits::complete_recurring_task_at_line(&lines.join("\n"), at, &today_iso_local()) {
            return Ok(Some((recurring.body, recurring.inserted_tasks)));
        }
    }
    line = match input.state {
        Some("open") => edits::set_task_in_progress_at_index(&edits::set_task_checked_at_index(&line, 0, false), 0, false),
        Some("done") => edits::set_task_checked_at_index(&line, 0, true),
        Some("in-progress") => edits::set_task_in_progress_at_index(&line, 0, true),
        Some("cancelled") => edits::set_task_cancelled_at_index(&line, 0, true),
        _ => line,
    };
    lines[at] = format!("{line}{ending}");
    Ok(Some((lines.join("\n"), 0)))
}

/// TaskNotes' priority vocabulary, which a task note's frontmatter speaks.
fn task_file_priority(priority: &str) -> Option<&'static str> {
    match priority {
        "high" => Some("high"),
        "med" => Some("normal"),
        "low" => Some("low"),
        _ => None,
    }
}

/// A whole-note task, whose fields are its frontmatter.
fn update_file_task(start: &str, task: &VaultTask, input: &UpdateTaskInput) -> Result<String, String> {
    if input.text.is_some() {
        return Err("A task note has no line of text to change; use rename_note for its title.".into());
    }
    // State, status and waiting are all its one `status:` field.
    let claims = [input.state.is_some(), input.status.is_some(), input.waiting.is_some()];
    if claims.iter().filter(|c| **c).count() > 1 {
        return Err("A task note keeps state, status and waiting in its one frontmatter status: pass only one of them.".into());
    }
    let crlf = uses_crlf(start);
    let mut body = if crlf { start.replace("\r\n", "\n") } else { start.to_string() };
    let today = today_iso_local();
    match input.state {
        Some("open") => body = edits::set_task_file_status(&body, false, &today),
        // Already finished keeps the day it was finished.
        Some("done") if !task.checked => body = edits::set_task_file_status(&body, true, &today),
        Some("in-progress") => body = edits::set_task_file_in_progress(&body, true),
        Some("cancelled") => body = edits::set_task_file_cancelled(&body, true),
        _ => {}
    }
    if let Some(due) = input.due {
        body = edits::update_frontmatter_fields(&body, &[("due", Some(due).filter(|d| !d.is_empty()))]);
    }
    if let Some(priority) = input.priority {
        body = edits::update_frontmatter_fields(&body, &[("priority", task_file_priority(priority))]);
    }
    match input.waiting {
        Some(true) => body = edits::update_frontmatter_fields(&body, &[("status", Some("waiting")), ("completedDate", None)]),
        Some(false) if task.waiting => body = edits::update_frontmatter_fields(&body, &[("status", Some("open"))]),
        _ => {}
    }
    if let Some(status) = input.status {
        if status.to_lowercase() == "done" {
            body = edits::set_task_file_status(&body, true, &today);
        } else {
            body = edits::update_frontmatter_fields(
                &body,
                &[("status", Some(status).filter(|s| !s.is_empty())), ("completedDate", None)],
            );
        }
    }
    Ok(if crlf { body.replace('\n', "\r\n") } else { body })
}

/// Edits one task through the backend's own read and write, so it takes a
/// scope the way any other edit does. The task as it now reads, or None when
/// the id no longer matches one.
fn update_task(backend: &dyn Backend, input: &UpdateTaskInput) -> Result<Option<VaultTask>, String> {
    validate_update_task(input)?;
    let (rel, index_str) = split_task_id(input.id)?;
    let Some(task) = find_task(backend, &rel, &index_str)? else { return Ok(None) };
    if let Some(expected) = input.expected_text {
        if js_trim(&task.content) != js_trim(expected) {
            return Err(format!(
                "Task {} now reads {}, not {}: the note changed. Run list_tasks again for its id.",
                input.id,
                go_quote(&task.content),
                go_quote(expected)
            ));
        }
    }
    let note = backend.read_note(&task.source_path)?;
    let (next, index) = if index_str == "task" {
        (update_file_task(&note.body, &task, input)?, index_str)
    } else {
        let Some((edited, inserted)) = update_task_line(&note.body, &task, input)? else { return Ok(None) };
        let moved = index_str.parse::<usize>().map(|i| (i + inserted).to_string()).unwrap_or_else(|_| "NaN".into());
        (edited, moved)
    };
    if next != note.body {
        backend.write_note(&task.source_path, &next)?;
    }
    find_task(backend, &task.source_path, &index)
}

/* ---------- daily_note ------------------------------------------------- */

/// `path.posix.dirname`.
fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => ".",
    }
}

fn daily_note(backend: &dyn Backend, kind: PeriodicKind, at: NaiveDateTime) -> Result<Value, String> {
    let raw = backend.vault_settings();
    let primary = ops::primary_notes_location(backend.root());
    let notes = backend.list_notes()?;
    let date = at.date();
    let plan = periodic::plan_daily_note(kind, date, &raw, &primary);
    let mut result = json!({ "kind": plan.kind, "date": plan.date, "enabled": plan.enabled });
    let refs: Vec<NoteRef> =
        notes.iter().map(|n| NoteRef { path: &n.path, title: &n.title, folder: &n.folder }).collect();
    // A note made under an older naming pattern still counts.
    if let Some(i) = periodic::find_daily_note(kind, date, &raw, &primary, &refs) {
        let found = &notes[i];
        result["exists"] = json!(true);
        result["path"] = json!(found.path);
        result["note"] = serde_json::to_value(found).map_err(|e| e.to_string())?;
        return Ok(result);
    }
    result["exists"] = json!(false);
    // A folder this client cannot see stays unnamed, and starting the note
    // anywhere else would only misfile it.
    if let Some(scoped) = backend.scoped() {
        if !(scoped.shares_path(dirname(&plan.rel)) && scoped.shares_path(&plan.rel)) {
            result["cannotCreate"] = json!("This note's folder is not shared with this assistant, so it cannot be created here.");
            return Ok(result);
        }
    }
    // The vault's template for this kind when the client may read it, else a
    // heading. A template that cannot be read leaves the heading.
    let custom = match plan.template_id {
        Some(_) => backend.list_templates().ok(),
        None => None,
    };
    let body = periodic::periodic_body(plan.template_id.as_deref(), custom.as_deref(), &plan.title, at);
    result["createWith"] = json!({ "folder": "inbox", "subpath": plan.subpath, "title": plan.title, "body": body });
    Ok(result)
}

/* ---------- Tools ------------------------------------------------------ */

/// The work tools, or None for another tool.
pub fn call_work_tool(name: &str, args: &Args, backend: &dyn Backend) -> Option<Result<Value, String>> {
    let run = || -> Result<Value, String> {
        match name {
            "update_task" => {
                let input = UpdateTaskInput {
                    id: require_string(args, "id")?,
                    expected_text: optional_string(args, "expectedText")?,
                    due: optional_string(args, "due")?,
                    priority: optional_string(args, "priority")?,
                    waiting: optional_boolean(args, "waiting")?,
                    state: optional_string(args, "state")?,
                    status: optional_string(args, "status")?,
                    text: optional_string(args, "text")?,
                };
                serde_json::to_value(update_task(backend, &input)?).map_err(|e| e.to_string())
            }
            "daily_note" => {
                let kind_name = optional_string(args, "kind")?.unwrap_or("");
                let kind = PeriodicKind::from_arg(kind_name).ok_or(periodic::KIND_ERROR)?;
                // In the time zone of the machine running the server, as the app does.
                let now = Local::now().naive_local();
                let at = periodic::resolve_date(optional_string(args, "date")?.unwrap_or(""), now)?;
                daily_note(backend, kind, at)
            }
            "note_sections" => {
                let rel = require_string(args, "path")?;
                let heading = optional_string(args, "heading")?.unwrap_or("");
                let line = optional_number(args, "lineNumber")?;
                let note = backend.read_note(rel)?;
                if js_trim(heading).is_empty() {
                    return Ok(json!({ "path": note.meta.path, "headings": note_sections(&note.body) }));
                }
                let section = find_section(&note.body, heading, line.map(|l| l as i64))?;
                Ok(json!({
                    "path": note.meta.path,
                    "heading": section.text,
                    "level": section.level,
                    "lineNumber": section.line_number,
                    "endLineNumber": section.end_line_number,
                    "body": section_body(&note.body, &section),
                }))
            }
            "edit_section" => {
                let rel = require_string(args, "path")?;
                let heading = require_string(args, "heading")?;
                let mode = require_string(args, "mode")?;
                let text = args.get("text").and_then(Value::as_str).ok_or("text must be a string")?;
                let mode = match mode {
                    "append" | "prepend" if js_trim(text).is_empty() => {
                        return Err("text is empty; only replace can clear a section".into());
                    }
                    "append" => SectionEditMode::Append,
                    "prepend" => SectionEditMode::Prepend,
                    "replace" => SectionEditMode::Replace,
                    _ => return Err("mode must be append, prepend or replace".into()),
                };
                let line = optional_number(args, "lineNumber")?;
                let note = backend.read_note(rel)?;
                let section = find_section(&note.body, heading, line.map(|l| l as i64))?;
                let edited = edit_section_in_body(&note.body, &section, mode, text);
                serde_json::to_value(backend.write_note(&note.meta.path, &edited)?).map_err(|e| e.to_string())
            }
            _ => unreachable!(),
        }
    };
    matches!(name, "update_task" | "daily_note" | "note_sections" | "edit_section").then(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_read_like_upstream() {
        assert!(is_valid_iso_date("2026-02-28"));
        assert!(!is_valid_iso_date("2026-02-30"));
        assert!(!is_valid_iso_date("2026-2-3"));
    }
}
