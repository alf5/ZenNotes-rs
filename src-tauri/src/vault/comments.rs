//! Note comment sidecar files (`.zennotes/comments/<rel>.comments.json`) —
//! port of `readNoteComments`/`writeNoteComments`/`normalizeNoteComment(s)`
//! and the move/copy helpers from apps/desktop/src/main/vault.ts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::ipc::types::NoteComment;
use crate::vault::layout::INTERNAL_VAULT_DIR;
use crate::vault::notes::to_posix;

const NOTE_COMMENTS_DIR: &str = "comments";
pub const NOTE_COMMENTS_SUFFIX: &str = ".comments.json";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn comments_root(root: &Path) -> PathBuf {
    root.join(INTERNAL_VAULT_DIR).join(NOTE_COMMENTS_DIR)
}

pub fn comments_path(root: &Path, rel: &str) -> PathBuf {
    comments_root(root).join(format!("{}{}", to_posix(rel), NOTE_COMMENTS_SUFFIX))
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn num(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64).or_else(|| {
        value.get(key).and_then(Value::as_f64).map(|f| f.floor() as i64)
    })
}

/// Port of `normalizeNoteComment`. Returns None when the body is empty.
fn normalize_one(input: &Value, note_path: &str) -> Option<NoteComment> {
    let body = input.get("body").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if body.is_empty() {
        return None;
    }
    let now = now_ms();
    let raw_start = num(input, "anchorStart").filter(|v| *v >= 0).unwrap_or(0).max(0);
    let raw_end = num(input, "anchorEnd").filter(|v| *v >= 0).unwrap_or(raw_start).max(0);
    let anchor_start = raw_start.min(raw_end);
    let anchor_end = raw_start.max(raw_end);
    let anchor_text = input
        .get("anchorText")
        .and_then(Value::as_str)
        .map(|s| collapse_ws(s).chars().take(500).collect())
        .unwrap_or_default();
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created_at = num(input, "createdAt").unwrap_or(now);
    let updated_at = num(input, "updatedAt").unwrap_or(now);
    let resolved_at = num(input, "resolvedAt");
    Some(NoteComment {
        id,
        note_path: note_path.to_string(),
        anchor_start,
        anchor_end,
        anchor_text,
        body,
        created_at,
        updated_at,
        resolved_at,
    })
}

/// Port of `normalizeNoteComments` — accepts an array or `{comments: [...]}`,
/// dedupes by id, sorts by createdAt then id.
pub fn normalize(raw: &Value, note_path: &str) -> Vec<NoteComment> {
    let values: Vec<Value> = if let Some(arr) = raw.as_array() {
        arr.clone()
    } else if let Some(arr) = raw.get("comments").and_then(Value::as_array) {
        arr.clone()
    } else {
        Vec::new()
    };
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<NoteComment> = Vec::new();
    for value in &values {
        if !value.is_object() {
            continue;
        }
        if let Some(c) = normalize_one(value, note_path) {
            if seen.insert(c.id.clone()) {
                out.push(c);
            }
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    out
}

pub fn read_note_comments(root: &Path, rel: &str) -> Vec<NoteComment> {
    let note_path = to_posix(rel);
    match fs::read_to_string(comments_path(root, &note_path)) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) => normalize(&value, &note_path),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

pub fn write_note_comments(root: &Path, rel: &str, comments: &Value) -> Result<Vec<NoteComment>, String> {
    let note_path = to_posix(rel);
    let normalized = normalize(comments, &note_path);
    let abs = comments_path(root, &note_path);
    if normalized.is_empty() {
        let _ = fs::remove_file(&abs);
        return Ok(Vec::new());
    }
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let payload = serde_json::json!({ "version": 1, "comments": normalized });
    let body = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    fs::write(&abs, body).map_err(|e| format!("write failed: {e}"))?;
    Ok(normalized)
}

pub fn remove_note_comments(root: &Path, rel: &str) {
    let _ = fs::remove_file(comments_path(root, rel));
}

/// Relocate a note's comments on rename/move, merging into any existing
/// destination sidecar. Mirrors `moveNoteComments`.
pub fn move_note_comments(root: &Path, old_rel: &str, new_rel: &str) {
    let old_abs = comments_path(root, old_rel);
    let new_abs = comments_path(root, new_rel);
    if old_abs == new_abs || !old_abs.exists() {
        return;
    }
    if let Some(parent) = new_abs.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if new_abs.exists() {
        let existing = read_note_comments(root, new_rel);
        let moving = read_note_comments(root, old_rel);
        let merged: Vec<Value> = existing
            .into_iter()
            .chain(moving)
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
            .collect();
        let _ = write_note_comments(root, new_rel, &Value::Array(merged));
        let _ = fs::remove_file(&old_abs);
    } else {
        let _ = fs::rename(&old_abs, &new_abs);
    }
}

/// Copy a note's comments to a duplicate, assigning fresh ids/timestamps.
/// Mirrors `copyNoteComments`.
pub fn copy_note_comments(root: &Path, src_rel: &str, dst_rel: &str) {
    let source = read_note_comments(root, src_rel);
    if source.is_empty() {
        return;
    }
    let now = now_ms();
    let copies: Vec<Value> = source
        .into_iter()
        .map(|mut c| {
            c.id = uuid::Uuid::new_v4().to_string();
            c.note_path = to_posix(dst_rel);
            c.created_at = now;
            c.updated_at = now;
            serde_json::to_value(c).unwrap_or(Value::Null)
        })
        .collect();
    let _ = write_note_comments(root, dst_rel, &Value::Array(copies));
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
    fn write_read_roundtrip_normalizes() {
        let v = vault();
        let input = serde_json::json!([
            { "notePath": "inbox/A.md", "anchorStart": 10, "anchorEnd": 5, "anchorText": "  some   text ", "body": " hi " },
            { "notePath": "inbox/A.md", "anchorStart": 0, "anchorEnd": 0, "anchorText": "x", "body": "   " }
        ]);
        let written = write_note_comments(v.path(), "inbox/A.md", &input).unwrap();
        // Empty-body comment dropped.
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].anchor_start, 5);
        assert_eq!(written[0].anchor_end, 10);
        assert_eq!(written[0].anchor_text, "some text");
        assert_eq!(written[0].body, "hi");

        let read = read_note_comments(v.path(), "inbox/A.md");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, written[0].id);
    }

    #[test]
    fn empty_write_removes_file() {
        let v = vault();
        let input = serde_json::json!([{ "notePath": "inbox/A.md", "anchorStart": 0, "anchorEnd": 0, "anchorText": "", "body": "keep" }]);
        write_note_comments(v.path(), "inbox/A.md", &input).unwrap();
        assert!(comments_path(v.path(), "inbox/A.md").exists());
        write_note_comments(v.path(), "inbox/A.md", &serde_json::json!([])).unwrap();
        assert!(!comments_path(v.path(), "inbox/A.md").exists());
    }

    #[test]
    fn copy_reassigns_ids() {
        let v = vault();
        let input = serde_json::json!([{ "id": "orig", "notePath": "inbox/A.md", "anchorStart": 0, "anchorEnd": 0, "anchorText": "x", "body": "c" }]);
        write_note_comments(v.path(), "inbox/A.md", &input).unwrap();
        copy_note_comments(v.path(), "inbox/A.md", "inbox/B.md");
        let copied = read_note_comments(v.path(), "inbox/B.md");
        assert_eq!(copied.len(), 1);
        assert_ne!(copied[0].id, "orig");
        assert_eq!(copied[0].note_path, "inbox/B.md");
    }
}
