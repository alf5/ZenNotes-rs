//! Comment threads for the MCP tools, ported from upstream
//! apps/desktop/src/mcp/comment-ops.ts and the threading helpers it uses from
//! shared-domain's note-comments.ts (`threadNoteComments`, `threadRootOf`,
//! `resolveCommentAnchor`, `lineOfOffset`). The shapes are what a model sees:
//! threads (a top-level comment with its replies), the anchored text and the
//! line it sits on today, and who said what.
//!
//! Anchor offsets are JavaScript string indices (UTF-16 code units), the
//! unit the app stores them in, so they are converted at the edges here.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use super::ops;
use crate::ipc::types::NoteComment;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentView {
    id: String,
    author: Option<String>,
    body: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentThreadView {
    #[serde(flatten)]
    comment: CommentView,
    /// The text the comment was written on, as stored. Empty for a note-level comment.
    anchor_text: String,
    /// 1-based line the anchor sits on in the note as it is now.
    line: usize,
    resolved: bool,
    resolved_at: Option<i64>,
    replies: Vec<CommentView>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn view(c: &NoteComment) -> CommentView {
    CommentView {
        id: c.id.clone(),
        author: c.author.clone(),
        body: c.body.clone(),
        created_at: c.created_at,
        updated_at: c.updated_at,
    }
}

/// The top-level comment a comment's `parent_id` chain ends at.
fn root_index(comments: &[NoteComment], index: usize) -> usize {
    let mut current = index;
    let mut visited = std::collections::HashSet::new();
    while let Some(parent) = &comments[current].parent_id {
        if !visited.insert(current) {
            break;
        }
        match comments.iter().position(|c| &c.id == parent) {
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

/// Group comments into threads, each a top-level comment with its replies in
/// order; a reply to a reply lands in the same thread.
fn threads(comments: &[NoteComment]) -> Vec<(usize, Vec<usize>)> {
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..comments.len() {
        if root_index(comments, i) == i {
            out.push((i, Vec::new()));
        }
    }
    for i in 0..comments.len() {
        let root = root_index(comments, i);
        if root != i {
            if let Some(thread) = out.iter_mut().find(|(r, _)| *r == root) {
                thread.1.push(i);
            }
        }
    }
    out
}

fn thread_root_of<'a>(comments: &'a [NoteComment], id: &str) -> Option<&'a NoteComment> {
    threads(comments)
        .into_iter()
        .find(|(root, replies)| comments[*root].id == id || replies.iter().any(|r| comments[*r].id == id))
        .map(|(root, _)| &comments[root])
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn index_of(haystack: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Where a comment's anchor sits in `doc` now: the stored offsets when the
/// text there still matches, else the first occurrence of the anchored text,
/// else the (clamped) stored offsets. Returns the start, in UTF-16 units.
fn resolve_anchor_from(c: &NoteComment, doc: &[u16]) -> usize {
    let len = doc.len() as i64;
    let from = c.anchor_start.clamp(0, len) as usize;
    let to = c.anchor_end.clamp(from as i64, len) as usize;
    let anchor = utf16(&c.anchor_text);
    if c.anchor_text.is_empty() || doc[from..to] == anchor[..] {
        return from;
    }
    index_of(doc, &anchor).unwrap_or(from)
}

/// 1-based line number of a UTF-16 offset.
fn line_of_offset(doc: &[u16], offset: usize) -> usize {
    1 + doc[..offset.min(doc.len())].iter().filter(|u| **u == 10).count()
}

pub fn list_threads(root: &Path, rel: &str, include_resolved: bool) -> Result<Vec<CommentThreadView>, String> {
    let note = ops::read_note(root, rel)?;
    let comments = ops::read_note_comments(root, rel);
    let doc = utf16(&note.body);
    Ok(threads(&comments)
        .into_iter()
        .filter(|(r, _)| include_resolved || comments[*r].resolved_at.is_none())
        .map(|(r, replies)| {
            let c = &comments[r];
            CommentThreadView {
                comment: view(c),
                anchor_text: c.anchor_text.clone(),
                line: line_of_offset(&doc, resolve_anchor_from(c, &doc)),
                resolved: c.resolved_at.is_some(),
                resolved_at: c.resolved_at,
                replies: replies.iter().map(|i| view(&comments[*i])).collect(),
            }
        })
        .collect())
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where a new comment attaches, as (start, end, text) in UTF-16 units.
/// `anchor_text` must appear in the note as written (an exact match first,
/// then one ignoring case); without it the comment is note-level, at the top.
fn anchor_for_text(doc: &str, anchor_text: Option<&str>) -> Result<(usize, usize, String), String> {
    let wanted = anchor_text.unwrap_or("").trim();
    if wanted.is_empty() {
        return Ok((0, 0, String::new()));
    }
    let doc16 = utf16(doc);
    let wanted16 = utf16(wanted);
    let at = index_of(&doc16, &wanted16)
        .or_else(|| index_of(&utf16(&doc.to_lowercase()), &utf16(&wanted.to_lowercase())))
        .ok_or_else(|| {
            "anchor_text was not found in the note. Pass the text exactly as it appears (read_note shows it), or omit it for a note-level comment.".to_string()
        })?;
    let end = (at + wanted16.len()).min(doc16.len());
    let text: String = collapse_ws(&String::from_utf16_lossy(&doc16[at..end])).chars().take(500).collect();
    Ok((at, at + wanted16.len(), text))
}

fn as_values(comments: &[NoteComment]) -> Vec<Value> {
    comments.iter().map(|c| serde_json::to_value(c).unwrap_or(Value::Null)).collect()
}

fn thread_by_id(root: &Path, rel: &str, id: &str, what: &str) -> Result<CommentThreadView, String> {
    list_threads(root, rel, true)?
        .into_iter()
        .find(|t| t.comment.id == id)
        .ok_or_else(|| format!("Thread {id} vanished while {what}"))
}

pub fn add_comment(
    root: &Path,
    rel: &str,
    body: &str,
    anchor_text: Option<&str>,
    author: &str,
) -> Result<CommentThreadView, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("body must not be empty".into());
    }
    let note = ops::read_note(root, rel)?;
    let (start, end, text) = anchor_for_text(&note.body, anchor_text)?;
    let now = now_ms();
    let mut list = as_values(&ops::read_note_comments(root, rel));
    list.push(json!({
        "notePath": rel, "anchorStart": start, "anchorEnd": end, "anchorText": text,
        "body": body, "author": author, "createdAt": now, "updatedAt": now, "resolvedAt": null
    }));
    let written = ops::write_note_comments(root, rel, &list)?;
    let created = written
        .iter()
        .find(|c| c.created_at == now && c.body == body)
        .or(written.last())
        .map(|c| c.id.clone())
        .unwrap_or_default();
    let mut all = list_threads(root, rel, true)?;
    match all.iter().position(|t| t.comment.id == created) {
        Some(i) => Ok(all.swap_remove(i)),
        None => all.pop().ok_or_else(|| "The comment was not saved".to_string()),
    }
}

fn no_comment(id: &str, rel: &str) -> String {
    format!("No comment with id {id} on {rel}. Use list_comments to find ids.")
}

pub fn reply_to_comment(root: &Path, rel: &str, id: &str, body: &str, author: &str) -> Result<CommentThreadView, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("body must not be empty".into());
    }
    let current = ops::read_note_comments(root, rel);
    let thread_root = thread_root_of(&current, id).ok_or_else(|| no_comment(id, rel))?;
    let now = now_ms();
    let mut list = as_values(&current);
    list.push(json!({
        "notePath": rel, "anchorStart": thread_root.anchor_start, "anchorEnd": thread_root.anchor_end,
        "anchorText": thread_root.anchor_text, "body": body, "author": author,
        "parentId": thread_root.id, "createdAt": now, "updatedAt": now, "resolvedAt": null
    }));
    let root_id = thread_root.id.clone();
    ops::write_note_comments(root, rel, &list)?;
    thread_by_id(root, rel, &root_id, "replying")
}

pub fn resolve_comment(root: &Path, rel: &str, id: &str, resolved: bool) -> Result<CommentThreadView, String> {
    let current = ops::read_note_comments(root, rel);
    let root_id = thread_root_of(&current, id).ok_or_else(|| no_comment(id, rel))?.id.clone();
    let now = now_ms();
    let list: Vec<Value> = current
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
            if c.id == root_id {
                v["resolvedAt"] = if resolved { json!(now) } else { Value::Null };
                v["updatedAt"] = json!(now);
            }
            v
        })
        .collect();
    ops::write_note_comments(root, rel, &list)?;
    thread_by_id(root, rel, &root_id, "resolving")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_use_utf16_offsets() {
        let (start, end, text) = anchor_for_text("😀 Key\nSentence here", Some("key\nsentence")).unwrap();
        assert_eq!((start, end, text.as_str()), (3, 15, "Key Sentence"));
        assert_eq!(anchor_for_text("abc", None).unwrap(), (0, 0, String::new()));
        assert!(anchor_for_text("abc", Some("zzz")).is_err());
        let doc = utf16("a\nb\nc");
        assert_eq!(line_of_offset(&doc, 4), 3);
    }
}
