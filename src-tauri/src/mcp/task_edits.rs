//! Pure text edits behind the MCP `update_task` tool and the recurring-task
//! branch of `toggle_task`: a port of the shared-domain functions upstream's
//! `updateTaskLine` / `updateFileTask` (apps/desktop/src/mcp/work-tools.ts) and
//! `toggleTaskBody` (apps/desktop/src/mcp/vault-ops.ts) call, from
//! packages/shared-domain/src/ (vendored in this repo):
//! - task-recurrence.ts: `completeRecurringTaskAtIndex`,
//!   `completeRecurringTaskAtLine` and everything they reach (`parseRepeatRule`,
//!   `nextRepeatDate`, `addDaysIso` and the other calendar helpers,
//!   `repeatFieldOf`, `planRecurringCompletion`, `tidy`, `bare`).
//! - tasklists.ts: `setTask{Text,Due,Priority,Waiting,Field,Checked,InProgress,
//!   Cancelled}AtIndex` (through `editTaskAtIndex` / `toggleTaskAtIndex`), plus
//!   `TASK_LINE_RE`, `FENCE_RE`, `taskLineNumbers`, `leadingIndentWidth` and
//!   `taskBlockEnd`.
//! - frontmatter.ts: `updateFrontmatterFields`, `setTaskFileStatus`,
//!   `setTaskFileCancelled`, `setTaskFileInProgress`, `yamlValue`, `unquote`.
//! - tasks.ts: `noteFrontmatterDue` (`parseNoteDefaults`' `due:`,
//!   `normalizeDueDate`, `isValidIsoDate`).
//!
//! Note these are the APP's task grammar (`@shared/tasklists`), not the MCP
//! server's own copy in tasks.rs: the regexes differ (FENCE_RE here is
//! `/^(\s*)(```|~~~)/`, any indent, exactly three marker characters).
//!
//! Every regex is a JS regex re-spelled for Rust, under the rules in the
//! `//!` header of tasks.rs:
//! - JS `\s` / `trim` is ECMAScript WhiteSpace + LineTerminator, not Unicode
//!   White_Space: `ws!()` / `is_js_ws`. JS `\d` is ASCII: `[0-9]`. JS `.`
//!   also stops at `\r`, U+2028 and U+2029: `dot!()`.
//! - A `\b` after a letter is `(?![A-Za-z0-9_])` (ASCII, outside any case
//!   folding).
//! - Case folding: an `iu` regex folds by Unicode simple case folding, which
//!   is what Rust's `(?i)` does. An `i` regex WITHOUT `u` folds ASCII only (a
//!   non-ASCII character never canonicalizes to an ASCII one), so `@status:`
//!   must not match `@ſtatus:` (U+017F) there; those are spelled with explicit
//!   `[sS]` classes (`ascii_ci`) instead of `(?i)`.
//! - `String.prototype.replace` templates (`'$1'`, `` `$1due:${nextDue}` ``)
//!   are expanded by `get_substitution`, the spec's GetSubstitution, so a `$`
//!   in an interpolated value behaves as it does upstream.
//! - Lengths and slices are taken on byte offsets of matches or on ASCII
//!   prefixes, where they agree with JS's UTF-16 offsets.
//!
//! Dates follow JS `Date` arithmetic exactly, not a calendar library:
//! `Date.UTC` maps years 0-99 to 1900-1999 and rolls months and days over
//! (`2026-13-40` is a usable due date that means 2027-02-09), a time value
//! past +-8.64e15 ms is NaN and prints `NaN-NaN-NaN`, and `>` on dates is
//! UTF-16 string comparison.
//!
//! One upstream quirk is mirrored on purpose: `NAMED[value]` in
//! `parseRepeatRule` is a plain object lookup, so `@repeat:constructor`,
//! `@repeat:after-constructor` and `@repeat:after-__proto__` find
//! `Object.prototype` members and yield an interval rule with no unit and no
//! count, whose next date is `NaN-NaN-NaN` (see `RepeatRule::PrototypeKey`).

use std::cmp::Ordering;
use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

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
/// The `FIELD_VALUE` body of task-recurrence.ts, `[\p{L}\d][\p{L}\d/_-]*`.
macro_rules! field_value {
    () => {
        r"[\p{L}0-9][\p{L}0-9/_-]*"
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

/// `s.replace(/\s+$/u, '')`.
fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_ws)
}

/// `s.replace(re, ' ')` for a global `re` matching a run of two or more of
/// the characters `is_run` accepts: `/\s{2,}/g` or `/[ \t]{2,}/g`. A lone
/// character of the class stays as it is.
fn collapse_runs(s: &str, is_run: fn(char) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if !is_run(c) {
            out.push(c);
            continue;
        }
        let mut end = start + c.len_utf8();
        let mut count = 1;
        while let Some(&(i, next)) = chars.peek() {
            if !is_run(next) {
                break;
            }
            end = i + next.len_utf8();
            count += 1;
            chars.next();
        }
        if count >= 2 {
            out.push(' ');
        } else {
            out.push_str(&s[start..end]);
        }
    }
    out
}

/// `/\s{2,}/g` → `' '`.
fn collapse_ws(s: &str) -> String {
    collapse_runs(s, is_js_ws)
}

/// An `i`-without-`u` literal: each ASCII letter as `[xX]`. The input is a
/// literal of letters, digits, `_` and `-`, none of them regex syntax.
fn ascii_ci(literal: &str) -> String {
    literal
        .chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static task-edit regex")
}

/// The first match's captures, or `None`. These patterns are linear and
/// bounded, so fancy-regex's backtrack limit is never the reason for a miss.
fn captures<'t>(regex: &Regex, text: &'t str) -> Option<Captures<'t>> {
    regex.captures(text).ok().flatten()
}

fn group<'t>(caps: &Captures<'t>, i: usize) -> &'t str {
    caps.get(i).map(|m| m.as_str()).unwrap_or("")
}

fn is_match(regex: &Regex, text: &str) -> bool {
    regex.is_match(text).unwrap_or(false)
}

/// The spec's GetSubstitution: what `template` becomes for one match of a
/// regex without named groups. `$$`, `$&`, `` $` ``, `$'`, `$n` and `$nn`
/// (falling back to one digit when `nn` names no group); anything else,
/// `$<` included, is literal.
fn get_substitution(caps: &Captures, haystack: &str, template: &str) -> String {
    let whole = caps.get(0).expect("group 0 is the match");
    let groups = caps.len() - 1;
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let simple = match bytes[i + 1] {
                b'$' => Some("$"),
                b'&' => Some(whole.as_str()),
                b'`' => Some(&haystack[..whole.start()]),
                b'\'' => Some(&haystack[whole.end()..]),
                _ => None,
            };
            if let Some(text) = simple {
                out.push_str(text);
                i += 2;
                continue;
            }
            if bytes[i + 1].is_ascii_digit() {
                let first = usize::from(bytes[i + 1] - b'0');
                let mut digits = 1;
                let mut index = first;
                if i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit() {
                    let two = first * 10 + usize::from(bytes[i + 2] - b'0');
                    if two <= groups {
                        digits = 2;
                        index = two;
                    }
                }
                if (1..=groups).contains(&index) {
                    out.push_str(caps.get(index).map(|m| m.as_str()).unwrap_or(""));
                } else {
                    out.push_str(&template[i..i + 1 + digits]);
                }
                i += 1 + digits;
                continue;
            }
        }
        let c = template[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// `s.replace(regex, template)` for a regex without the `g` flag.
fn js_replace(regex: &Regex, s: &str, template: &str) -> String {
    match captures(regex, s) {
        Some(caps) => {
            let m = caps.get(0).expect("group 0 is the match");
            let mut out = String::with_capacity(s.len());
            out.push_str(&s[..m.start()]);
            out.push_str(&get_substitution(&caps, s, template));
            out.push_str(&s[m.end()..]);
            out
        }
        None => s.to_string(),
    }
}

/// `s.replace(regex, template)` for a regex WITH the `g` flag. None of the
/// patterns here can match the empty string.
fn js_replace_all(regex: &Regex, s: &str, template: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in regex.captures_iter(s).map_while(Result::ok) {
        let m = caps.get(0).expect("group 0 is the match");
        out.push_str(&s[last..m.start()]);
        out.push_str(&get_substitution(&caps, s, template));
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

/// JS `a > b` on strings: UTF-16 code unit order.
fn js_gt(a: &str, b: &str) -> bool {
    a.encode_utf16().cmp(b.encode_utf16()) == Ordering::Greater
}

/* ---------- tasklists.ts: the task line and the index walk ------------- */

/// `/^(\s*(?:>\s*)*(?:[-+*]|\d+[.)])\s+\[)( |x|X|>|-|\/)(\].*)$/`: group 1 the
/// prefix through `[`, group 2 the state char, group 3 `]` and the rest.
static TASK_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        "^([", ws!(), "]*(?:>[", ws!(), "]*)*(?:[-+*]|[0-9]+[.)])[", ws!(), r"]+\[)( |x|X|>|-|/)(\]",
        dot!(),
        "*)$"
    ))
});
/// `/^(\s*)(```|~~~)/`; the marker is group 1 here.
static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| re(concat!("^[", ws!(), "]*(```|~~~)")));

/// A TASK_LINE_RE match: `match[1]`, `match[2]` and `match[3]`.
struct TaskMatch<'t> {
    prefix: &'t str,
    state: &'t str,
    /// Starts with `]`.
    tail: &'t str,
}

fn task_match(line: &str) -> Option<TaskMatch<'_>> {
    let caps = captures(&TASK_LINE_RE, line)?;
    Some(TaskMatch { prefix: group(&caps, 1), state: group(&caps, 2), tail: group(&caps, 3) })
}

fn fence_marker(line: &str) -> Option<&str> {
    captures(&FENCE_RE, line).map(|caps| group(&caps, 1))
}

/// `bareLine` / `bare`: the line without the `\r` a Windows file ends it with.
fn bare_line(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn is_open_state(state: &str) -> bool {
    state == " " || state == "/"
}

/// Fence tracking shared by the index walks: true when `line` is a fence
/// line or inside a fence, i.e. never a task.
fn skip_fenced<'a>(fence: &mut Option<&'a str>, line: &'a str) -> bool {
    if let Some(marker) = fence_marker(line) {
        match *fence {
            None => *fence = Some(marker),
            Some(open) if open == marker => *fence = None,
            Some(_) => {}
        }
        return true;
    }
    fence.is_some()
}

/// `editTaskAtIndex`: hand the task line at `task_index` (fence-aware, CRLF
/// lines matched bare) to `mutate` and splice the result back in, keeping the
/// line's ending. Unchanged when `mutate` gives `None` or the same line, or the
/// index is out of range.
fn edit_task_at_index(
    markdown: &str,
    task_index: usize,
    mutate: impl FnOnce(&TaskMatch) -> Option<String>,
) -> String {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut current = 0;
    let mut fence = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = bare_line(raw);
        if skip_fenced(&mut fence, line) {
            continue;
        }
        let Some(task) = task_match(line) else { continue };
        if current != task_index {
            current += 1;
            continue;
        }
        let Some(next) = mutate(&task) else { return markdown.to_string() };
        if next == line {
            return markdown.to_string();
        }
        let replaced = format!("{next}{}", &raw[line.len()..]);
        let mut out: Vec<&str> = lines.clone();
        out[i] = &replaced;
        return out.join("\n");
    }
    markdown.to_string()
}

/// `taskLineNumbers`: the line of every task, indexed by task index.
fn task_line_numbers(lines: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut fence = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = bare_line(raw);
        if skip_fenced(&mut fence, line) {
            continue;
        }
        if task_match(line).is_some() {
            out.push(i);
        }
    }
    out
}

/// `leadingIndentWidth`: `/^[ \t]*/` length.
fn leading_indent_width(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ' || *b == b'\t').count()
}

/// `taskBlockEnd`: the exclusive end of the indented block under the task on
/// line `start`. A blank line does not end it while deeper content follows;
/// a dedent, a fence or the end of the note does, leaving trailing blanks.
fn task_block_end(lines: &[&str], start: usize, base_indent: usize) -> usize {
    let mut end = start + 1;
    while end < lines.len() {
        let next = lines[end];
        if js_trim(next).is_empty() {
            let mut j = end + 1;
            while j < lines.len() && js_trim(lines[j]).is_empty() {
                j += 1;
            }
            if j >= lines.len()
                || is_match(&FENCE_RE, lines[j])
                || leading_indent_width(lines[j]) <= base_indent
            {
                break;
            }
            end = j;
            continue;
        }
        if is_match(&FENCE_RE, next) || leading_indent_width(next) <= base_indent {
            break;
        }
        end += 1;
    }
    end
}

/* ---------- tasklists.ts: the setters ---------------------------------- */

/// tasklists' `/(^|\s)@waiting\b/i` (no `g`).
static WAITING_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!("(^|[{}])@{}(?![A-Za-z0-9_])", ws!(), ascii_ci("waiting")))
});
/// `/(^|\s)!(?:high|med|medium|low|h|m|l)\b/i`. Alternation order matters:
/// `!medium` first tries `med`, fails the boundary, then backtracks.
static PRIORITY_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    let words: Vec<String> =
        ["high", "med", "medium", "low", "h", "m", "l"].iter().map(|w| ascii_ci(w)).collect();
    re(&format!("(^|[{}])!(?:{})(?![A-Za-z0-9_])", ws!(), words.join("|")))
});
/// tasklists' `/(^|\s)due:\s*\S+/i`.
static DUE_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!("(^|[{ws}]){}:[{ws}]*[^{ws}]+", ascii_ci("due"), ws = ws!()))
});

/// `FIELD_KEY_RE`, `/^[a-z][a-z0-9_-]*$/`.
fn is_field_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// `setTaskTextAtIndex`: everything after the checkbox becomes `text`,
/// trimmed (verbatim otherwise: the caller owns any tokens). A blank `text`
/// leaves a bare `- [ ]`.
pub fn set_task_text_at_index(body: &str, index: usize, text: &str) -> String {
    edit_task_at_index(body, index, |task| {
        task.tail.strip_prefix(']')?;
        let trimmed = js_trim(text);
        let tail = if trimmed.is_empty() { String::new() } else { format!(" {trimmed}") };
        Some(format!("{}{}]{tail}", task.prefix, task.state))
    })
}

/// `setTaskDueAtIndex`: replace, append or (with `None`, or an empty string,
/// which JS reads as false) remove the `due:` token.
pub fn set_task_due_at_index(body: &str, index: usize, due: Option<&str>) -> String {
    edit_task_at_index(body, index, |task| {
        let tail = task.tail.strip_prefix(']')?;
        let cleaned = collapse_ws(&js_replace(&DUE_TOKEN_RE, tail, "$1"));
        let next_tail = match due.filter(|d| !d.is_empty()) {
            Some(due) => format!("{} due:{due}", js_trim_end(&cleaned)),
            None => js_trim_end(&cleaned).to_string(),
        };
        Some(format!("{}{}]{next_tail}", task.prefix, task.state))
    })
}

/// `setTaskPriorityAtIndex`: replace, append or (with `None` / empty) remove
/// the `!high` / `!med` / `!low` token. The value is written as given.
pub fn set_task_priority_at_index(body: &str, index: usize, priority: Option<&str>) -> String {
    edit_task_at_index(body, index, |task| {
        let tail = task.tail.strip_prefix(']')?;
        let cleaned = collapse_ws(&js_replace(&PRIORITY_TOKEN_RE, tail, "$1"));
        let next_tail = match priority.filter(|p| !p.is_empty()) {
            Some(priority) => format!("{} !{priority}", js_trim_end(&cleaned)),
            None => js_trim_end(&cleaned).to_string(),
        };
        Some(format!("{}{}]{next_tail}", task.prefix, task.state))
    })
}

/// `setTaskWaitingAtIndex`: append `@waiting` when absent, or remove the first
/// one and tidy the whitespace it leaves.
pub fn set_task_waiting_at_index(body: &str, index: usize, waiting: bool) -> String {
    edit_task_at_index(body, index, |task| {
        let tail = task.tail.strip_prefix(']')?;
        let has = is_match(&WAITING_TOKEN_RE, tail);
        let next_tail = if waiting && !has {
            format!("{} @waiting", js_trim_end(tail))
        } else if !waiting && has {
            js_trim_end(&collapse_ws(&js_replace(&WAITING_TOKEN_RE, tail, "$1"))).to_string()
        } else {
            tail.to_string()
        };
        Some(format!("{}{}]{next_tail}", task.prefix, task.state))
    })
}

/// `setTaskFieldAtIndex`: replace, append or (with `None` / empty) remove the
/// `@<key>:<value>` token. A key that is not a lowercase slug leaves the body
/// unchanged.
pub fn set_task_field_at_index(body: &str, index: usize, key: &str, value: Option<&str>) -> String {
    if !is_field_key(key) {
        return body.to_string();
    }
    // `new RegExp(`(^|\\s)@${key}:\\S+`, 'i')`.
    let token_re = re(&format!("(^|[{ws}])@{}:[^{ws}]+", ascii_ci(key), ws = ws!()));
    edit_task_at_index(body, index, |task| {
        let tail = task.tail.strip_prefix(']')?;
        let cleaned = collapse_ws(&js_replace(&token_re, tail, "$1"));
        let next_tail = match value.filter(|v| !v.is_empty()) {
            Some(value) => format!("{} @{key}:{value}", js_trim_end(&cleaned)),
            None => js_trim_end(&cleaned).to_string(),
        };
        Some(format!("{}{}]{next_tail}", task.prefix, task.state))
    })
}

/// `setTaskCheckedAtIndex` (= `toggleTaskAtIndex`): `[x]` when checking,
/// `[ ]` when not, except that unchecking keeps an in-progress `[/]` (#599).
pub fn set_task_checked_at_index(body: &str, index: usize, checked: bool) -> String {
    edit_task_at_index(body, index, |task| {
        if !checked && task.state == "/" {
            return Some(format!("{}/{}", task.prefix, task.tail));
        }
        Some(format!("{}{}{}", task.prefix, if checked { 'x' } else { ' ' }, task.tail))
    })
}

/// `setTaskInProgressAtIndex`: `[/]`, or back to `[ ]`.
pub fn set_task_in_progress_at_index(body: &str, index: usize, in_progress: bool) -> String {
    edit_task_at_index(body, index, |task| {
        Some(format!("{}{}{}", task.prefix, if in_progress { '/' } else { ' ' }, task.tail))
    })
}

/// `setTaskCancelledAtIndex`: `[-]`, or back to `[ ]`.
pub fn set_task_cancelled_at_index(body: &str, index: usize, cancelled: bool) -> String {
    edit_task_at_index(body, index, |task| {
        Some(format!("{}{}{}", task.prefix, if cancelled { '-' } else { ' ' }, task.tail))
    })
}

/* ---------- frontmatter.ts --------------------------------------------- */

/// `/^---\n([\s\S]*?)\n---\n?/`, run on a CRLF-normalized body (both
/// `updateFrontmatterFields` and tasks.ts' `FRONTMATTER_RE`).
static FRONTMATTER_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^---\n([\s\S]*?)\n---\n?"));
/// `/^\s*([A-Za-z0-9_][\w-]*)\s*:/` — the key of a frontmatter line.
static FM_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!("^[", ws!(), "]*([A-Za-z0-9_][A-Za-z0-9_-]*)[", ws!(), "]*:"))
});

/// `unquote`: trim, then strip one layer of matching quotes.
fn unquote(v: &str) -> &str {
    let trimmed = js_trim(v);
    let bytes = trimmed.as_bytes();
    // Both ends must be one-byte ASCII quotes, so byte length 2 is UTF-16
    // length 2 whenever it matters.
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            return &trimmed[1..trimmed.len() - 1];
        }
    }
    trimmed
}

/// `yamlValue`: the scalar bare, or as a JSON string whenever a YAML reader
/// could misread it bare. serde_json escapes exactly what `JSON.stringify`
/// does for a valid string (`"`, `\`, the short escapes, other C0 controls as
/// lowercase `\u00xx`; not DEL, U+2028 or U+2029).
fn yaml_value(value: &str) -> String {
    let mut chars = value.chars();
    let leading_indicator =
        matches!(chars.next(), Some('-' | '?')) && chars.next().is_none_or(is_js_ws);
    if value.contains([':', '#', '"', '\'', '\n'])
        || value.starts_with(['[', ']', '{', '}', ',', '>', '|', '&', '*', '!', '%', '@', '`'])
        || leading_indicator
        || js_trim(value) != value
        || value.is_empty()
    {
        return serde_json::to_string(value).expect("a string serializes");
    }
    value.to_string()
}

/// A JS array index (`"0"`, or no leading zero and below 2^32 - 1), which an
/// object enumerates before its other keys, ascending.
fn array_index(key: &str) -> Option<u64> {
    if key == "0" {
        return Some(0);
    }
    if key.is_empty() || key.len() > 10 || key.starts_with('0') || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u64>().ok().filter(|n| *n < 4_294_967_295)
}

/// `Object.entries` of an object literal written with `fields` in order: a
/// repeated key keeps its first place and takes its last value, and array
/// indices come first, ascending.
fn object_entries<'a>(fields: &[(&'a str, Option<&'a str>)]) -> Vec<(&'a str, Option<&'a str>)> {
    let mut unique: Vec<(&str, Option<&str>)> = Vec::with_capacity(fields.len());
    for &(key, value) in fields {
        match unique.iter_mut().find(|entry| entry.0 == key) {
            Some(entry) => entry.1 = value,
            None => unique.push((key, value)),
        }
    }
    let mut indexed: Vec<(&str, Option<&str>)> =
        unique.iter().copied().filter(|entry| array_index(entry.0).is_some()).collect();
    indexed.sort_by_key(|entry| array_index(entry.0));
    indexed.extend(unique.into_iter().filter(|entry| array_index(entry.0).is_none()));
    indexed
}

/// `updateFrontmatterFields`: set, or with `None` remove, flat scalar fields,
/// keeping every other line byte for byte. Keys match case-insensitively and
/// are written as given; fields the block lacks go at its end, and a note
/// without a block gets one from the non-`None` fields. `fields` is the JS
/// object's entries in insertion order (`None` = `null`). Like upstream, a body
/// that gets edited comes back with `\r\n` normalized to `\n`.
pub fn update_frontmatter_fields(body: &str, fields: &[(&str, Option<&str>)]) -> String {
    let entries = object_entries(fields);
    if entries.is_empty() {
        return body.to_string();
    }
    let norm = body.replace("\r\n", "\n");
    // `new Map(entries.map(([k, v]) => [k.toLowerCase(), { key: k, value: v }]))`:
    // a key seen again keeps its place and takes the later key and value.
    let mut remaining: Vec<(String, &str, Option<&str>)> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let lower = key.to_lowercase();
        match remaining.iter_mut().find(|entry| entry.0 == lower) {
            Some(entry) => {
                entry.1 = key;
                entry.2 = value;
            }
            None => remaining.push((lower, key, value)),
        }
    }
    let field_line = |key: &str, value: &str| format!("{key}: {}", yaml_value(value));

    let Some(caps) = captures(&FRONTMATTER_RE, &norm) else {
        let mut lines = vec!["---".to_string()];
        for (_, key, value) in &remaining {
            if let Some(value) = value {
                lines.push(field_line(key, value));
            }
        }
        if lines.len() == 1 {
            return body.to_string();
        }
        lines.push("---".to_string());
        lines.push(String::new());
        return lines.join("\n") + &norm;
    };
    let block_end = caps.get(0).expect("group 0 is the match").end();
    let mut out: Vec<String> = Vec::new();
    for line in group(&caps, 1).split('\n') {
        if let Some(key_caps) = captures(&FM_KEY_RE, line) {
            let lower = group(&key_caps, 1).to_lowercase();
            if let Some(at) = remaining.iter().position(|entry| entry.0 == lower) {
                let (_, key, value) = remaining.remove(at);
                // A null value drops the line.
                if let Some(value) = value {
                    out.push(field_line(key, value));
                }
                continue;
            }
        }
        out.push(line.to_string());
    }
    for (_, key, value) in &remaining {
        if let Some(value) = value {
            out.push(field_line(key, value));
        }
    }
    format!("---\n{}\n---\n{}", out.join("\n"), &norm[block_end..])
}

/// `setTaskFileStatus`: `status: done` and `completedDate: <today_iso>`, or
/// back to `status: open` without a `completedDate`.
pub fn set_task_file_status(body: &str, done: bool, today_iso: &str) -> String {
    update_frontmatter_fields(
        body,
        &[
            ("status", Some(if done { "done" } else { "open" })),
            ("completedDate", if done { Some(today_iso) } else { None }),
        ],
    )
}

/// `setTaskFileCancelled`: `status: cancelled`, or back to `open`; never a
/// `completedDate` (#450).
pub fn set_task_file_cancelled(body: &str, cancelled: bool) -> String {
    update_frontmatter_fields(
        body,
        &[("status", Some(if cancelled { "cancelled" } else { "open" })), ("completedDate", None)],
    )
}

/// `setTaskFileInProgress`: `status: in-progress` (TaskNotes' spelling), or
/// back to `open`; never a `completedDate` (#512).
pub fn set_task_file_in_progress(body: &str, in_progress: bool) -> String {
    update_frontmatter_fields(
        body,
        &[("status", Some(if in_progress { "in-progress" } else { "open" })), ("completedDate", None)],
    )
}

/* ---------- tasks.ts: the note-wide due date --------------------------- */

/// `YYYY-MM-DD` as three numbers: `/^(\d{4})-(\d{2})-(\d{2})$/`.
fn iso_fields(iso: &str) -> Option<(i64, i64, i64)> {
    let b = iso.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| {
        b[range].iter().try_fold(0i64, |acc, c| c.is_ascii_digit().then(|| acc * 10 + i64::from(c - b'0')))
    };
    Some((number(0..4)?, number(5..7)?, number(8..10)?))
}

/// tasks.ts `isValidIsoDate`: the shape plus
/// `Number.isFinite(Date.parse(`${s}T00:00:00Z`))`, which accepts any day
/// 01-31 in any month 01-12 (Feb 31 rolls over) and year 0000.
fn is_valid_iso_date(s: &str) -> bool {
    iso_fields(s).is_some_and(|(_, month, day)| (1..=12).contains(&month) && (1..=31).contains(&day))
}

fn normalize_due_date(raw: &str) -> Option<&str> {
    if raw.is_empty() {
        return None;
    }
    let cleaned = unquote(js_trim(raw));
    is_valid_iso_date(cleaned).then_some(cleaned)
}

/// `noteFrontmatterDue`: the last valid `due:` of the leading frontmatter.
fn note_frontmatter_due(body: &str) -> Option<String> {
    let norm = body.replace("\r\n", "\n");
    let caps = captures(&FRONTMATTER_RE, &norm)?;
    let mut due = None;
    for raw_line in group(&caps, 1).split('\n') {
        let line = js_trim(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else { continue };
        if colon < 1 || js_trim(&line[..colon]).to_lowercase() != "due" {
            continue;
        }
        // `parseNoteDefaults` unquotes the value, then `normalizeDueDate`
        // unquotes it again.
        if let Some(date) = normalize_due_date(unquote(&line[colon + 1..])) {
            due = Some(date.to_string());
        }
    }
    due
}

/* ---------- task-recurrence.ts: calendar dates ------------------------- */

/// A JS time value in whole days since the epoch (every value here is a UTC
/// midnight); `None` is NaN.
type JsDay = Option<i64>;

/// `TimeClip`: |t| <= 8.64e15 ms, i.e. 1e8 days.
fn time_clip(day: i64) -> JsDay {
    (day.abs() <= 100_000_000).then_some(day)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The `(year, month 1-12, day)` of a day number.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `Date.UTC(year, month, date)`: years 0-99 mean 1900-1999, and months and
/// days roll over.
fn date_utc(year: i64, month: i64, date: i64) -> JsDay {
    let year = if (0..=99).contains(&year) { 1900 + year } else { year };
    let ym = year + month.div_euclid(12);
    time_clip(days_from_civil(ym, month.rem_euclid(12) + 1, 1) + date - 1)
}

/// `isoToUtc`.
fn iso_to_utc(iso: &str) -> JsDay {
    let (year, month, day) = iso_fields(iso)?;
    date_utc(year, month - 1, day)
}

/// `utcToIso`.
fn utc_to_iso(t: JsDay) -> String {
    match t {
        None => "NaN-NaN-NaN".to_string(),
        Some(days) => {
            let (year, month, day) = civil_from_days(days);
            format!("{year}-{month:02}-{day:02}")
        }
    }
}

/// `addDaysIso`: `iso` moved by `n` days, or unchanged when not a date.
fn add_days_iso(iso: &str, n: i64) -> String {
    match iso_to_utc(iso) {
        None => iso.to_string(),
        Some(t) => utc_to_iso(time_clip(t + n)),
    }
}

/// `addMonthsIso`: `iso` moved by `n` months, the day clamped to the month.
fn add_months_iso(iso: &str, n: i64) -> String {
    let Some(t) = iso_to_utc(iso) else { return iso.to_string() };
    let (year, month1, day) = civil_from_days(t);
    let month = month1 - 1 + n;
    let year = year + month.div_euclid(12);
    let m = month.rem_euclid(12);
    // `new Date(Date.UTC(year, m + 1, 0)).getUTCDate()`; NaN poisons the rest.
    let Some(last) = date_utc(year, m + 1, 0) else { return utc_to_iso(None) };
    utc_to_iso(date_utc(year, m, day.min(civil_from_days(last).2)))
}

/// `weekdayOf`: 0 = Sunday; 0 for a non-date.
fn weekday_of(iso: &str) -> i64 {
    iso_to_utc(iso).map_or(0, |t| (t + 4).rem_euclid(7))
}

/* ---------- task-recurrence.ts: rules ---------------------------------- */

/// `RepeatUnit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatUnit {
    Day,
    Week,
    Month,
    Year,
}

/// `RepeatRule`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepeatRule {
    /// Every `every` units, counted from the due date, or from the day the
    /// task is finished when `after_done` (`@repeat:after-3d`).
    Interval { unit: RepeatUnit, every: i64, after_done: bool },
    /// On these weekdays (0 = Sunday, ascending): `mon/thu`, `weekdays`.
    Days { days: Vec<i64> },
    /// Upstream's `{ kind: 'interval', afterDone }` with no `unit` or
    /// `every`: what `NAMED[value]` gives for `constructor` and `__proto__`,
    /// which name `Object.prototype` members. Its next date is `NaN-NaN-NaN`.
    PrototypeKey { after_done: bool },
}

const DAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// `parseRepeatRule`: the rule a `@repeat:` value names, or `None`.
fn parse_repeat_rule(raw: Option<&str>) -> Option<RepeatRule> {
    let raw = raw.filter(|raw| !raw.is_empty())?;
    let lowered = js_trim(raw).to_lowercase();
    let mut value = lowered.as_str();
    if value == "weekdays" {
        return Some(RepeatRule::Days { days: vec![1, 2, 3, 4, 5] });
    }
    let mut after_done = false;
    if let Some(rest) = value.strip_prefix("after-") {
        after_done = true;
        value = rest;
    }
    let named = match value {
        "daily" => Some(RepeatUnit::Day),
        "weekly" => Some(RepeatUnit::Week),
        "monthly" => Some(RepeatUnit::Month),
        "yearly" | "annually" => Some(RepeatUnit::Year),
        "constructor" | "__proto__" => return Some(RepeatRule::PrototypeKey { after_done }),
        _ => None,
    };
    if let Some(unit) = named {
        return Some(RepeatRule::Interval { unit, every: 1, after_done });
    }
    // INTERVAL_RE, `/^(\d{1,3})([dwmy])$/`.
    let bytes = value.as_bytes();
    if (2..=4).contains(&bytes.len()) && bytes[..bytes.len() - 1].iter().all(u8::is_ascii_digit) {
        let unit = match bytes[bytes.len() - 1] {
            b'd' => Some(RepeatUnit::Day),
            b'w' => Some(RepeatUnit::Week),
            b'm' => Some(RepeatUnit::Month),
            b'y' => Some(RepeatUnit::Year),
            _ => None,
        };
        if let Some(unit) = unit {
            let every: i64 = value[..value.len() - 1].parse().expect("1-3 ASCII digits");
            if every < 1 {
                return None;
            }
            return Some(RepeatRule::Interval { unit, every, after_done });
        }
    }
    if after_done {
        return None;
    }
    let mut days = Vec::new();
    for part in value.split('/') {
        let day = DAY_NAMES.iter().position(|name| *name == part)? as i64;
        if !days.contains(&day) {
            days.push(day);
        }
    }
    days.sort_unstable();
    (!days.is_empty()).then_some(RepeatRule::Days { days })
}

fn add_units(iso: &str, n: i64, unit: RepeatUnit) -> String {
    match unit {
        RepeatUnit::Day => add_days_iso(iso, n),
        RepeatUnit::Week => add_days_iso(iso, n * 7),
        RepeatUnit::Month => add_months_iso(iso, n),
        RepeatUnit::Year => add_months_iso(iso, n * 12),
    }
}

/// `nextRepeatDate`: the first date of the rule's series after both `base`
/// (the due date) and `today`, so a late finish skips what it missed. Months
/// count from `base` each time; `after_done` rules count from `today`.
fn next_repeat_date(rule: &RepeatRule, base: &str, today: &str) -> String {
    match rule {
        RepeatRule::Days { days } => {
            let mut date = add_days_iso(if js_gt(base, today) { base } else { today }, 1);
            let mut i = 0;
            while i < 7 && !days.contains(&weekday_of(&date)) {
                date = add_days_iso(&date, 1);
                i += 1;
            }
            date
        }
        // `addMonthsIso(x, NaN)` is `x` for a non-date and NaN otherwise, and
        // both of upstream's branches return that one value.
        RepeatRule::PrototypeKey { after_done } => {
            let from = if *after_done { today } else { base };
            if iso_to_utc(from).is_none() { from.to_string() } else { utc_to_iso(None) }
        }
        RepeatRule::Interval { unit, every, after_done: true } => add_units(today, *every, *unit),
        RepeatRule::Interval { unit: unit @ (RepeatUnit::Day | RepeatUnit::Week), every, .. } => {
            let step = every * if *unit == RepeatUnit::Week { 7 } else { 1 };
            match (iso_to_utc(base), iso_to_utc(today)) {
                (Some(base_t), Some(today_t)) => {
                    let behind = today_t - base_t;
                    let k = if behind < 0 { 1 } else { behind / step + 1 };
                    add_days_iso(base, k * step)
                }
                _ => add_days_iso(base, step),
            }
        }
        RepeatRule::Interval { unit, every, .. } => {
            let months = every * if *unit == RepeatUnit::Year { 12 } else { 1 };
            // Every `addMonthsIso(base, …)` of a non-date is `base`, and both
            // exits of upstream's loop then return it.
            if iso_to_utc(base).is_none() {
                return base.to_string();
            }
            for k in 1..100_000 {
                let date = add_months_iso(base, k * months);
                if js_gt(&date, today) {
                    return date;
                }
            }
            add_months_iso(base, months)
        }
    }
}

/* ---------- task-recurrence.ts: the check-off edit --------------------- */

/// `/(?:^|\s)@repeat:(FIELD_VALUE)/iu`.
static REPEAT_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!("(?i)(?:^|[", ws!(), "])@repeat:(", field_value!(), ")"))
});
/// `/(^|\s)@repeat:FIELD_VALUE/giu`.
static REPEAT_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("(?i)(^|[", ws!(), "])@repeat:", field_value!())));
/// `/(^|\s)@remind:FIELD_VALUE/giu`.
static REMIND_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("(?i)(^|[", ws!(), "])@remind:", field_value!())));
/// `/(^|\s)@status:FIELD_VALUE/giu`.
static STATUS_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("(?i)(^|[", ws!(), "])@status:", field_value!())));
// task-recurrence's WAITING_TOKEN_RE, `/(^|\s)@waiting\b/gi`, is the same
// pattern as tasklists' WAITING_TOKEN_RE, but used with `g` (js_replace_all).
/// task-recurrence's `/(^|\s)due:\s*(\S+)/i`: the date is group 2.
static DUE_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!("(^|[{ws}]){}:[{ws}]*([^{ws}]+)", ascii_ci("due"), ws = ws!()))
});

/// `repeatFieldOf`: the `@repeat:` value of a task's text, lower-cased.
fn repeat_field_of(text: &str) -> Option<String> {
    captures(&REPEAT_FIELD_RE, text).map(|caps| group(&caps, 1).to_lowercase())
}

/// `tidy`: `/[ \t]{2,}/g` → `' '`, then the trailing whitespace off.
fn tidy(text: &str) -> String {
    js_trim_end(&collapse_runs(text, |c| c == ' ' || c == '\t')).to_string()
}

/// `RecurringCompletionOptions`.
struct RecurringOptions<'a> {
    /// The day the task is finished, `YYYY-MM-DD` in the user's time zone.
    today: &'a str,
    /// The date a task without a `due:` falls back on.
    fallback_due: Option<&'a str>,
    /// "Skip this time": the record is written cancelled (`[-]`).
    skip: bool,
}

/// `RecurringLinePlan`.
struct LinePlan {
    next: Vec<String>,
    record: String,
    record_children: Vec<String>,
    replaces: usize,
    next_due: String,
    rule: RepeatRule,
}

/// `planRecurringCompletion`: the occurrence that goes above the open
/// recurring task on line `at`, and what its own block becomes. `None` when
/// that line is not an open (`[ ]` / `[/]`) task with a rule.
fn plan_recurring_completion(lines: &[&str], at: usize, opts: &RecurringOptions) -> Option<LinePlan> {
    let line = bare_line(lines.get(at)?);
    let task = task_match(line)?;
    if !is_open_state(task.state) {
        return None;
    }
    let rest = task.tail.strip_prefix(']')?;
    let rule = parse_repeat_rule(repeat_field_of(rest).as_deref())?;

    let written = captures(&DUE_VALUE_RE, rest).map(|caps| group(&caps, 2));
    let written_due = written.filter(|date| iso_to_utc(date).is_some());
    let base = written_due.or(opts.fallback_due).unwrap_or(opts.today);
    let next_due = next_repeat_date(&rule, base, opts.today);

    // The occurrence keeps every token in place, `due:` replaced where it
    // stood; `@waiting` and `@status:` start fresh.
    let next_rest = if is_match(&DUE_VALUE_RE, rest) {
        js_replace(&DUE_VALUE_RE, rest, &format!("$1due:{next_due}"))
    } else {
        format!("{} due:{next_due}", js_trim_end(rest))
    };
    let next_rest = tidy(&js_replace_all(
        &STATUS_TOKEN_RE,
        &js_replace_all(&WAITING_TOKEN_RE, &next_rest, "$1"),
        "$1",
    ));
    let mut next = vec![format!("{} ]{next_rest}", task.prefix)];

    // The subtree comes along with every checkbox open again; the record
    // cancels the subtasks it left open.
    let bare: Vec<&str> = lines.iter().map(|l| bare_line(l)).collect();
    let end = task_block_end(&bare, at, leading_indent_width(line));
    let mut record_children = Vec::with_capacity(end - at - 1);
    for child in &bare[at + 1..end] {
        match task_match(child) {
            Some(sub) => {
                next.push(format!("{} {}", sub.prefix, sub.tail));
                record_children.push(if is_open_state(sub.state) {
                    format!("{}-{}", sub.prefix, sub.tail)
                } else {
                    child.to_string()
                });
            }
            None => {
                next.push(child.to_string());
                record_children.push(child.to_string());
            }
        }
    }

    let record_rest = tidy(&js_replace_all(
        &REMIND_TOKEN_RE,
        &js_replace_all(&REPEAT_TOKEN_RE, rest, "$1"),
        "$1",
    ));
    let record = format!("{}{}]{record_rest}", task.prefix, if opts.skip { '-' } else { 'x' });
    Some(LinePlan { next, record, record_children, replaces: end - at, next_due, rule })
}

/// `RecurringCompletion`: the edit checking off a recurring task makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurringCompletion {
    pub body: String,
    /// The new occurrence's `due:`.
    #[allow(dead_code)] // upstream returns it; the MCP tools read only body / inserted_tasks
    pub next_due: String,
    #[allow(dead_code)] // as next_due
    pub rule: RepeatRule,
    /// How many lines went in above the finished task.
    #[allow(dead_code)] // as next_due
    pub inserted_lines: usize,
    /// How many of them are tasks: the finished task's index moves down by
    /// this much.
    pub inserted_tasks: usize,
}

fn complete_recurring_task_at_index_with(
    markdown: &str,
    task_index: usize,
    opts: &RecurringOptions,
) -> Option<RecurringCompletion> {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let at = *task_line_numbers(&lines).get(task_index)?;
    complete_recurring_task_at_line_with(markdown, at, opts)
}

fn complete_recurring_task_at_line_with(
    markdown: &str,
    at: usize,
    opts: &RecurringOptions,
) -> Option<RecurringCompletion> {
    let lines: Vec<&str> = markdown.split('\n').collect();
    if at >= lines.len() {
        return None;
    }
    let frontmatter_due = note_frontmatter_due(markdown);
    let plan = plan_recurring_completion(
        &lines,
        at,
        &RecurringOptions {
            today: opts.today,
            fallback_due: frontmatter_due.as_deref().or(opts.fallback_due),
            skip: opts.skip,
        },
    )?;
    let ending_of = |line: &str| if line.ends_with('\r') { "\r" } else { "" };
    // Every inserted line takes the task's own ending; each record child keeps
    // the ending its line had.
    let ending = ending_of(lines[at]);
    let mut out: Vec<String> = lines[..at].iter().map(|l| l.to_string()).collect();
    out.extend(plan.next.iter().map(|l| format!("{l}{ending}")));
    out.push(format!("{}{ending}", plan.record));
    out.extend(
        plan.record_children.iter().enumerate().map(|(k, l)| format!("{l}{}", ending_of(lines[at + 1 + k]))),
    );
    out.extend(lines[at + plan.replaces..].iter().map(|l| l.to_string()));
    let inserted_tasks = plan.next.iter().filter(|l| task_match(l).is_some()).count();
    Some(RecurringCompletion {
        body: out.join("\n"),
        next_due: plan.next_due,
        rule: plan.rule,
        inserted_lines: plan.next.len(),
        inserted_tasks,
    })
}

/// `completeRecurringTaskAtIndex(body, index, { today })`: check off the open
/// recurring task at `index` (counted like the parser, past fenced code) —
/// its next occurrence goes above it, the task becomes its record. `None`
/// when that task is not an open recurring one. `\r\n` endings are kept.
pub fn complete_recurring_task_at_index(body: &str, index: usize, today: &str) -> Option<RecurringCompletion> {
    complete_recurring_task_at_index_with(body, index, &RecurringOptions { today, fallback_due: None, skip: false })
}

/// `completeRecurringTaskAtLine(body, line, { today })`: the same for the task
/// on a known 0-based line.
pub fn complete_recurring_task_at_line(body: &str, line: usize, today: &str) -> Option<RecurringCompletion> {
    complete_recurring_task_at_line_with(body, line, &RecurringOptions { today, fallback_due: None, skip: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    /* ---------- recurrence: the shared case table ---------------------- */

    struct Case {
        name: &'static str,
        body: &'static str,
        task_index: usize,
        today: &'static str,
        fallback_due: Option<&'static str>,
        skip: bool,
        expected: Option<&'static str>,
    }

    /// task-recurrence.cases.json, verbatim.
    const CASES: &[Case] = &[
        Case {
            name: "weekly from its due date, the record drops @repeat and @remind",
            body: "- [ ] Water the plants @repeat:weekly due:2026-11-01 @remind:9am",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Water the plants @repeat:weekly due:2026-11-08 @remind:9am\n- [x] Water the plants due:2026-11-01"),
        },
        Case {
            name: "a late finish skips the missed occurrences",
            body: "- [ ] Journal @repeat:daily due:2026-10-27",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Journal @repeat:daily due:2026-11-02\n- [x] Journal due:2026-10-27"),
        },
        Case {
            name: "an early finish goes one interval past the due date",
            body: "- [ ] Report @repeat:2w due:2026-11-10",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Report @repeat:2w due:2026-11-24\n- [x] Report due:2026-11-10"),
        },
        Case {
            name: "no due date counts from today and writes one",
            body: "- [ ] Stretch @repeat:3d",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Stretch @repeat:3d due:2026-11-04\n- [x] Stretch"),
        },
        Case {
            name: "a daily note's date stands in for a missing due",
            body: "- [ ] Review inbox @repeat:weekly",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: Some("2026-10-30"),
            skip: false,
            expected: Some("- [ ] Review inbox @repeat:weekly due:2026-11-06\n- [x] Review inbox"),
        },
        Case {
            name: "frontmatter due beats the daily note's date",
            body: "---\ndue: 2026-10-31\n---\n- [ ] Pay invoice @repeat:monthly",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: Some("2026-10-01"),
            skip: false,
            expected: Some("---\ndue: 2026-10-31\n---\n- [ ] Pay invoice @repeat:monthly due:2026-11-30\n- [x] Pay invoice"),
        },
        Case {
            name: "monthly clamps to the month's last day",
            body: "- [ ] Rent @repeat:monthly due:2027-01-31",
            task_index: 0,
            today: "2027-01-31",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Rent @repeat:monthly due:2027-02-28\n- [x] Rent due:2027-01-31"),
        },
        Case {
            name: "yearly across a leap day",
            body: "- [ ] Renew passport @repeat:yearly due:2028-02-29",
            task_index: 0,
            today: "2028-02-29",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Renew passport @repeat:yearly due:2029-02-28\n- [x] Renew passport due:2028-02-29"),
        },
        Case {
            name: "weekday lists pick the next listed day",
            body: "- [ ] Gym @repeat:mon/thu due:2026-11-02",
            task_index: 0,
            today: "2026-11-02",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Gym @repeat:mon/thu due:2026-11-05\n- [x] Gym due:2026-11-02"),
        },
        Case {
            name: "weekdays skip the weekend",
            body: "- [ ] Stand-up @repeat:weekdays due:2026-11-06",
            task_index: 0,
            today: "2026-11-06",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Stand-up @repeat:weekdays due:2026-11-09\n- [x] Stand-up due:2026-11-06"),
        },
        Case {
            name: "after- counts from the day it is finished",
            body: "- [ ] Water the fern @repeat:after-3d due:2026-10-20",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Water the fern @repeat:after-3d due:2026-11-04\n- [x] Water the fern due:2026-10-20"),
        },
        Case {
            name: "the subtree repeats unticked; the record keeps its ticks and cancels what was left open",
            body: "## Chores\n- [ ] Weekly review @repeat:fri due:2026-10-30\n  - [x] Inbox zero\n  - [ ] Plan the week\n  - [/] Draft the update\n    - [ ] Nested open\n  - [-] Calendar\n  - [>] Moved [[Elsewhere]]\n  notes stay\n- [ ] Other",
            task_index: 0,
            today: "2026-10-30",
            fallback_due: None,
            skip: false,
            expected: Some("## Chores\n- [ ] Weekly review @repeat:fri due:2026-11-06\n  - [ ] Inbox zero\n  - [ ] Plan the week\n  - [ ] Draft the update\n    - [ ] Nested open\n  - [ ] Calendar\n  - [ ] Moved [[Elsewhere]]\n  notes stay\n- [x] Weekly review due:2026-10-30\n  - [x] Inbox zero\n  - [-] Plan the week\n  - [-] Draft the update\n    - [-] Nested open\n  - [-] Calendar\n  - [>] Moved [[Elsewhere]]\n  notes stay\n- [ ] Other"),
        },
        Case {
            name: "in progress finishes, the next one starts open; @waiting and @status do not carry",
            body: "- [/] Ship it !high @repeat:weekly due:2026-11-01 @status:doing @waiting #work",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Ship it !high @repeat:weekly due:2026-11-08 #work\n- [x] Ship it !high due:2026-11-01 @status:doing @waiting #work"),
        },
        Case {
            name: "skip writes a cancelled record",
            body: "- [ ] Water the plants @repeat:weekly due:2026-11-01\n  - [ ] Check the soil",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: true,
            expected: Some("- [ ] Water the plants @repeat:weekly due:2026-11-08\n  - [ ] Check the soil\n- [-] Water the plants due:2026-11-01\n  - [-] Check the soil"),
        },
        Case {
            name: "numbered, quoted and indented tasks keep their prefix",
            body: "> 1. [ ] Quoted @repeat:daily due:2026-11-01",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("> 1. [ ] Quoted @repeat:daily due:2026-11-02\n> 1. [x] Quoted due:2026-11-01"),
        },
        Case {
            name: "the index counts like the parser, past fenced code",
            body: "```\n- [ ] not a task @repeat:daily\n```\n- [ ] first\n- [ ] second @repeat:daily due:2026-11-01",
            task_index: 1,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("```\n- [ ] not a task @repeat:daily\n```\n- [ ] first\n- [ ] second @repeat:daily due:2026-11-02\n- [x] second due:2026-11-01"),
        },
        Case {
            name: "a spaced due token is replaced as one",
            body: "- [ ] Spaced @repeat:daily due: 2026-11-01 tail",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Spaced @repeat:daily due:2026-11-02 tail\n- [x] Spaced due: 2026-11-01 tail"),
        },
        Case {
            name: "CRLF notes keep their endings",
            body: "- [ ] Win @repeat:daily due:2026-11-01\r\n  - [ ] sub\r\n- [ ] after\r\n",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Win @repeat:daily due:2026-11-02\r\n  - [ ] sub\r\n- [x] Win due:2026-11-01\r\n  - [-] sub\r\n- [ ] after\r\n"),
        },
        Case {
            name: "the rule is case-insensitive",
            body: "- [ ] Loud @Repeat:WEEKLY due:2026-11-01",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: Some("- [ ] Loud @Repeat:WEEKLY due:2026-11-08\n- [x] Loud due:2026-11-01"),
        },
        Case {
            name: "an unknown rule is an ordinary task",
            body: "- [ ] Odd @repeat:fortnightly due:2026-11-01",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: None,
        },
        Case {
            name: "a done task does not repeat again",
            body: "- [x] Done @repeat:daily due:2026-11-01",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: None,
        },
        Case {
            name: "cancelled and forwarded tasks do not repeat",
            body: "- [-] Cancelled @repeat:daily\n- [>] Forwarded @repeat:daily",
            task_index: 1,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: None,
        },
        Case {
            name: "a task without a rule is ordinary",
            body: "- [ ] Plain due:2026-11-01",
            task_index: 0,
            today: "2026-11-01",
            fallback_due: None,
            skip: false,
            expected: None,
        },
    ];

    #[test]
    fn shared_case_table() {
        for case in CASES {
            let opts = RecurringOptions { today: case.today, fallback_due: case.fallback_due, skip: case.skip };
            let got = complete_recurring_task_at_index_with(case.body, case.task_index, &opts);
            assert_eq!(got.map(|c| c.body).as_deref(), case.expected, "{}", case.name);
        }
    }

    fn rule(value: &str) -> RepeatRule {
        parse_repeat_rule(Some(value)).unwrap_or_else(|| panic!("{value} names a rule"))
    }

    fn interval(unit: RepeatUnit, every: i64, after_done: bool) -> RepeatRule {
        RepeatRule::Interval { unit, every, after_done }
    }

    #[test]
    fn completion_reports_what_it_inserted() {
        let done = complete_recurring_task_at_index(
            "- [ ] Water @repeat:weekly due:2026-11-01 @remind:9am",
            0,
            "2026-11-01",
        )
        .unwrap();
        assert_eq!(
            done.body,
            "- [ ] Water @repeat:weekly due:2026-11-08 @remind:9am\n- [x] Water due:2026-11-01"
        );
        assert_eq!(done.inserted_lines, 1);
        assert_eq!(done.inserted_tasks, 1);
        assert_eq!(done.next_due, "2026-11-08");
        assert_eq!(done.rule, interval(RepeatUnit::Week, 1, false));

        // A subtree of two subtasks and a note line: four lines, three tasks.
        let body = "- [ ] Review @repeat:weekly due:2026-11-01\n  - [x] Inbox\n  - [ ] Plan\n  a note\nafter";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(done.inserted_lines, 4);
        assert_eq!(done.inserted_tasks, 3);
        assert_eq!(
            done.body,
            "- [ ] Review @repeat:weekly due:2026-11-08\n  - [ ] Inbox\n  - [ ] Plan\n  a note\n\
             - [x] Review due:2026-11-01\n  - [x] Inbox\n  - [-] Plan\n  a note\nafter"
        );
    }

    #[test]
    fn toggle_task_index_follows_the_finished_task() {
        // toggleTaskBody returns `targetIndex + insertedTasks`: the record of
        // task #1 is now task #1 + 2.
        let body = "- [ ] a\n- [ ] b @repeat:daily due:2026-11-01\n  - [ ] sub\n- [ ] c";
        let done = complete_recurring_task_at_index(body, 1, "2026-11-01").unwrap();
        let lines: Vec<&str> = done.body.split('\n').collect();
        let record_line = task_line_numbers(&lines)[1 + done.inserted_tasks];
        assert_eq!(lines[record_line], "- [x] b due:2026-11-01");
    }

    #[test]
    fn subtree_follows_task_block_end() {
        // A blank line inside a loose list keeps the block going; a dedent
        // after trailing blanks ends it and leaves the blanks behind.
        let body = "- [ ] Plan @repeat:daily due:2026-11-01\n  - [ ] a\n\n  - [ ] b\n\nnext";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(
            done.body,
            "- [ ] Plan @repeat:daily due:2026-11-02\n  - [ ] a\n\n  - [ ] b\n\
             - [x] Plan due:2026-11-01\n  - [-] a\n\n  - [-] b\n\nnext"
        );
        assert_eq!(done.inserted_tasks, 3);
        // A fence closes the block.
        let body = "- [ ] Plan @repeat:daily due:2026-11-01\n  ```\n  - [ ] code\n  ```";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(done.inserted_lines, 1);
        // An indented task's subtree is what is deeper than it.
        let body = "- [ ] top\n  - [ ] mid @repeat:daily\n    - [x] deep\n  - [ ] sibling";
        let done = complete_recurring_task_at_index(body, 1, "2026-11-01").unwrap();
        assert_eq!(
            done.body,
            "- [ ] top\n  - [ ] mid @repeat:daily due:2026-11-02\n    - [ ] deep\n\
             \x20 - [x] mid\n    - [x] deep\n  - [ ] sibling"
        );
    }

    #[test]
    fn crlf_endings_of_record_children_are_their_own() {
        // Inserted lines take the task's ending; record children keep theirs.
        let body = "- [ ] a @repeat:daily due:2026-11-01\r\n  - [ ] lf child\n  - [x] crlf child\r\nend";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(
            done.body,
            "- [ ] a @repeat:daily due:2026-11-02\r\n  - [ ] lf child\r\n  - [ ] crlf child\r\n\
             - [x] a due:2026-11-01\r\n  - [-] lf child\n  - [x] crlf child\r\nend"
        );
    }

    #[test]
    fn rule_forms_and_their_next_dates() {
        let today = "2026-11-04"; // a Wednesday
        let cases = [
            ("daily", "2026-11-05"),
            ("weekly", "2026-11-11"),
            ("monthly", "2026-12-04"),
            ("yearly", "2027-11-04"),
            ("annually", "2027-11-04"),
            ("1d", "2026-11-05"),
            ("10d", "2026-11-14"),
            ("2w", "2026-11-18"),
            ("3m", "2027-02-04"),
            ("2y", "2028-11-04"),
            ("007d", "2026-11-11"),
            ("999d", "2029-07-30"),
            ("after-daily", "2026-11-05"),
            ("after-2w", "2026-11-18"),
            ("after-1m", "2026-12-04"),
            ("after-yearly", "2027-11-04"),
            ("weekdays", "2026-11-05"),
            ("mon", "2026-11-09"),
            ("sun/sat", "2026-11-07"),
            ("wed", "2026-11-11"),
            ("thu/mon/thu", "2026-11-05"),
        ];
        for (value, expected) in cases {
            let body = format!("- [ ] t @repeat:{value} due:{today}");
            let done = complete_recurring_task_at_index(&body, 0, today).unwrap_or_else(|| panic!("{value}"));
            assert_eq!(done.next_due, expected, "@repeat:{value}");
            assert!(done.body.starts_with(&format!("- [ ] t @repeat:{value} due:{expected}\n")), "{value}");
        }
    }

    #[test]
    fn every_day_and_every_week_count_from_the_due_date() {
        // Early finish: one step past the due date, not past today.
        assert_eq!(next_repeat_date(&rule("daily"), "2026-11-10", "2026-11-01"), "2026-11-11");
        assert_eq!(next_repeat_date(&rule("weekly"), "2026-11-10", "2026-11-01"), "2026-11-17");
        // Late finish: the first date of the series after today.
        assert_eq!(next_repeat_date(&rule("3d"), "2026-11-01", "2026-11-07"), "2026-11-10");
        assert_eq!(next_repeat_date(&rule("2w"), "2026-10-01", "2026-11-12"), "2026-11-26");
        // On the due date itself.
        assert_eq!(next_repeat_date(&rule("weekly"), "2026-11-01", "2026-11-01"), "2026-11-08");
    }

    #[test]
    fn monthly_end_of_month() {
        assert_eq!(next_repeat_date(&rule("monthly"), "2027-01-31", "2027-01-31"), "2027-02-28");
        assert_eq!(next_repeat_date(&rule("monthly"), "2028-01-31", "2028-01-31"), "2028-02-29");
        // Counted from the due date every time, so the clamp never compounds.
        assert_eq!(next_repeat_date(&rule("monthly"), "2027-01-31", "2027-03-05"), "2027-03-31");
        assert_eq!(next_repeat_date(&rule("monthly"), "2026-08-31", "2026-09-30"), "2026-10-31");
        assert_eq!(next_repeat_date(&rule("1m"), "2026-10-31", "2026-11-01"), "2026-11-30");
        assert_eq!(next_repeat_date(&rule("yearly"), "2028-02-29", "2028-03-01"), "2029-02-28");
        assert_eq!(next_repeat_date(&rule("after-1m"), "2020-01-01", "2026-01-31"), "2026-02-28");
    }

    #[test]
    fn when_done_versus_due_based() {
        // Due-based: from the due date. When done: from today.
        assert_eq!(next_repeat_date(&rule("3d"), "2026-10-31", "2026-11-01"), "2026-11-03");
        assert_eq!(next_repeat_date(&rule("after-3d"), "2026-10-31", "2026-11-01"), "2026-11-04");
        assert_eq!(next_repeat_date(&rule("after-3d"), "2026-12-25", "2026-11-01"), "2026-11-04");
        let body = "- [ ] fern @repeat:after-1w due:2026-12-25";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(done.body, "- [ ] fern @repeat:after-1w due:2026-11-08\n- [x] fern due:2026-12-25");
    }

    #[test]
    fn no_due_date_uses_frontmatter_then_today() {
        let done = complete_recurring_task_at_index("- [ ] stretch @repeat:weekly", 0, "2026-11-01").unwrap();
        assert_eq!(done.body, "- [ ] stretch @repeat:weekly due:2026-11-08\n- [x] stretch");
        // A malformed due token falls back too, and is replaced where it stood.
        let done = complete_recurring_task_at_index("- [ ] a due:tomorrow @repeat:daily x", 0, "2026-11-01").unwrap();
        assert_eq!(done.body, "- [ ] a due:2026-11-02 @repeat:daily x\n- [x] a due:tomorrow x");
        // Frontmatter `due:` (quoted, CRLF note, last valid one wins).
        let body = "---\r\ndue: \"2026-10-20\"\r\ndue: nope\r\n---\r\n- [ ] pay @repeat:weekly\r\n";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(
            done.body,
            "---\r\ndue: \"2026-10-20\"\r\ndue: nope\r\n---\r\n- [ ] pay @repeat:weekly due:2026-11-03\r\n- [x] pay\r\n"
        );
        // Double unquoting, as parseNoteDefaults then normalizeDueDate do.
        assert_eq!(note_frontmatter_due("---\ndue: \"'2026-10-20'\"\n---\n").as_deref(), Some("2026-10-20"));
        assert_eq!(note_frontmatter_due("---\nDue : 2026-02-31\n---\n").as_deref(), Some("2026-02-31"));
        assert_eq!(note_frontmatter_due("---\ndue: 2026-13-01\n---\n"), None);
        assert_eq!(note_frontmatter_due("no frontmatter\ndue: 2026-01-01"), None);
        // The fallback only applies without a usable written due.
        let opts = RecurringOptions { today: "2026-11-01", fallback_due: Some("2026-10-30"), skip: false };
        let done = complete_recurring_task_at_index_with("- [ ] x @repeat:daily due:2026-11-05", 0, &opts).unwrap();
        assert_eq!(done.next_due, "2026-11-06");
    }

    #[test]
    fn parse_repeat_rule_forms() {
        assert_eq!(rule("daily"), interval(RepeatUnit::Day, 1, false));
        assert_eq!(rule("Weekly"), interval(RepeatUnit::Week, 1, false));
        assert_eq!(rule("annually"), interval(RepeatUnit::Year, 1, false));
        assert_eq!(rule("6m"), interval(RepeatUnit::Month, 6, false));
        assert_eq!(rule("after-2w"), interval(RepeatUnit::Week, 2, true));
        assert_eq!(rule(" after-monthly "), interval(RepeatUnit::Month, 1, true));
        assert_eq!(rule("weekdays"), RepeatRule::Days { days: vec![1, 2, 3, 4, 5] });
        assert_eq!(rule("thu/mon/thu"), RepeatRule::Days { days: vec![1, 4] });
        assert_eq!(rule("SAT/sun"), RepeatRule::Days { days: vec![0, 6] });
        for bad in ["", "fortnightly", "0d", "000w", "2x", "after-mon", "after-weekdays", "mon/funday", "mon/", "1000d", "d", "after-"] {
            assert_eq!(parse_repeat_rule(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(parse_repeat_rule(None), None);
        // KELVIN SIGN lower-cases to `k`, as JS toLowerCase does.
        assert_eq!(rule("wee\u{212A}ly"), interval(RepeatUnit::Week, 1, false));
    }

    #[test]
    fn prototype_key_quirk_matches_upstream() {
        // Upstream: NAMED['constructor'] is Object, so the rule is
        // `{ kind: 'interval', afterDone: false }` and the date NaN.
        assert_eq!(rule("constructor"), RepeatRule::PrototypeKey { after_done: false });
        assert_eq!(rule("after-__proto__"), RepeatRule::PrototypeKey { after_done: true });
        assert_eq!(parse_repeat_rule(Some("tostring")), None);
        let done = complete_recurring_task_at_index("- [ ] x @repeat:constructor due:2026-11-01", 0, "2026-11-01").unwrap();
        assert_eq!(done.body, "- [ ] x @repeat:constructor due:NaN-NaN-NaN\n- [x] x due:2026-11-01");
        let done = complete_recurring_task_at_index("- [ ] x @repeat:after-__proto__", 0, "2026-11-01").unwrap();
        assert_eq!(done.body, "- [ ] x @repeat:after-__proto__ due:NaN-NaN-NaN\n- [x] x");
    }

    #[test]
    fn next_repeat_date_upstream_cases() {
        assert_eq!(next_repeat_date(&rule("monthly"), "2027-01-31", "2027-03-05"), "2027-03-31");
        assert_eq!(next_repeat_date(&rule("weekly"), "2026-10-26", "2026-11-02"), "2026-11-09");
        assert_eq!(next_repeat_date(&rule("daily"), "2026-11-01", "2026-11-01"), "2026-11-02");
        assert_eq!(next_repeat_date(&rule("mon"), "2026-10-05", "2026-11-02"), "2026-11-09");
        assert_eq!(next_repeat_date(&rule("daily"), "2026-03-07", "2026-03-07"), "2026-03-08");
        assert_eq!(next_repeat_date(&rule("weekly"), "2026-10-24", "2026-10-24"), "2026-10-31");
    }

    #[test]
    fn js_date_arithmetic() {
        // Date.UTC maps years 0-99 to the 1900s and rolls fields over.
        assert_eq!(add_days_iso("0050-01-01", 1), "1950-01-02");
        assert_eq!(add_days_iso("2026-13-40", 1), "2027-02-10");
        assert_eq!(add_days_iso("2026-00-00", 0), "2025-11-30");
        assert_eq!(add_days_iso("not a date", 3), "not a date");
        assert_eq!(add_days_iso("2026-12-31", 1), "2027-01-01");
        assert_eq!(add_days_iso("2026-03-01", -1), "2026-02-28");
        assert_eq!(add_months_iso("2026-01-31", 1), "2026-02-28");
        assert_eq!(add_months_iso("2026-01-31", 13), "2027-02-28");
        assert_eq!(add_months_iso("1999-12-15", 1), "2000-01-15");
        // Beyond TimeClip is NaN.
        assert_eq!(add_days_iso("2026-01-01", 200_000_000), "NaN-NaN-NaN");
        assert_eq!(add_months_iso("2026-01-01", 12 * 300_000), "NaN-NaN-NaN");
        assert_eq!(weekday_of("2026-11-01"), 0);
        assert_eq!(weekday_of("1970-01-01"), 4);
        assert_eq!(weekday_of("junk"), 0);
        assert_eq!(civil_from_days(days_from_civil(-5, 3, 1)), (-5, 3, 1));
        // A rolled-over due date is still a usable one.
        let done = complete_recurring_task_at_index("- [ ] x @repeat:daily due:2026-02-30", 0, "2026-01-01").unwrap();
        assert_eq!(done.body, "- [ ] x @repeat:daily due:2026-03-03\n- [x] x due:2026-02-30");
        assert!(js_gt("2026-11-02", "2026-11-01"));
        assert!(js_gt("NaN-NaN-NaN", "2026-11-01"));
        // U+FF61 sorts below U+1F600 in UTF-16, above it in UTF-8.
        assert!(!js_gt("\u{1F600}", "\u{FF61}"));
    }

    #[test]
    fn plan_for_an_editor_line() {
        let lines = ["- [ ] A @repeat:daily", "  - [x] done child", "  - [ ] open child", "after"];
        let opts = RecurringOptions { today: "2026-11-01", fallback_due: None, skip: false };
        let plan = plan_recurring_completion(&lines, 0, &opts).unwrap();
        assert_eq!(plan.next, ["- [ ] A @repeat:daily due:2026-11-02", "  - [ ] done child", "  - [ ] open child"]);
        assert_eq!(plan.record, "- [x] A");
        assert_eq!(plan.record_children, ["  - [x] done child", "  - [-] open child"]);
        assert_eq!(plan.replaces, 3);
    }

    #[test]
    fn complete_at_line_uses_the_line_not_a_count() {
        let today = "2099-01-01";
        let body = "    ```\n    - [ ] looks like a task\n    ```\n- [ ] water @repeat:daily due:2099-01-01\n";
        let done = complete_recurring_task_at_line(body, 3, today).unwrap();
        let lines: Vec<&str> = done.body.split('\n').collect();
        assert_eq!(lines[3], "- [ ] water @repeat:daily due:2099-01-02");
        assert!(lines[4].starts_with("- [x] water"));
        // update_task's own use: the line a scan pointed at.
        let body = "- [ ] plain\n- [x] water @repeat:daily due:2099-01-01\n";
        assert!(complete_recurring_task_at_line(body, 0, today).is_none());
        assert!(complete_recurring_task_at_line(body, 1, today).is_none());
        assert!(complete_recurring_task_at_line(body, 9, today).is_none());
        assert!(complete_recurring_task_at_index(body, 7, today).is_none());
    }

    #[test]
    fn waiting_and_status_tokens_reset_on_the_copy_only() {
        let body = "- [ ] a @waiting @WAITING @status:x @Status:y @waitingroom @repeat:daily due:2026-11-01";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(
            done.body,
            "- [ ] a @waitingroom @repeat:daily due:2026-11-02\n\
             - [x] a @waiting @WAITING @status:x @Status:y @waitingroom due:2026-11-01"
        );
        // `@remind` and every `@repeat` leave the record.
        let body = "- [ ] a @repeat:daily @remind:1d @repeat:weekly due:2026-11-01";
        let done = complete_recurring_task_at_index(body, 0, "2026-11-01").unwrap();
        assert_eq!(done.body.split('\n').nth(1), Some("- [x] a due:2026-11-01"));
    }

    #[test]
    fn replacement_template_is_get_substitution() {
        // A `$` in an interpolated date expands as upstream's replace would.
        let opts = RecurringOptions { today: "$&", fallback_due: None, skip: false };
        let done = complete_recurring_task_at_index_with("- [ ] a @repeat:daily due:x", 0, &opts).unwrap();
        assert_eq!(done.body, "- [ ] a @repeat:daily due: due:x\n- [x] a due:x");
        let caps = captures(&DUE_VALUE_RE, "pre due:1 post").unwrap();
        let sub = |t: &str| get_substitution(&caps, "pre due:1 post", t);
        assert_eq!(sub("[$1|$2|$$|$&|$`|$']"), "[ |1|$| due:1|pre| post]");
        assert_eq!(sub("$0$3$12$02$<x>$"), "$0$3 21$<x>$");
    }

    /* ---------- tasklists setters --------------------------------------- */

    #[test]
    fn checked_by_index_past_fences() {
        let md = "- [ ] a\n- [ ] b\n- [ ] c";
        assert_eq!(set_task_checked_at_index(md, 1, true), "- [ ] a\n- [x] b\n- [ ] c");
        let md = "- [ ] a\n```\n- [ ] inside\n```\n- [ ] b";
        assert_eq!(set_task_checked_at_index(md, 1, true), "- [ ] a\n```\n- [ ] inside\n```\n- [x] b");
        // A fence of the other marker does not close it; any indent opens one.
        let md = "   ~~~\n- [ ] x\n```\n- [ ] y\n~~~\n- [ ] z";
        assert_eq!(set_task_checked_at_index(md, 0, true), "   ~~~\n- [ ] x\n```\n- [ ] y\n~~~\n- [x] z");
        assert_eq!(set_task_checked_at_index("- [ ] only one", 5, true), "- [ ] only one");
        assert_eq!(set_task_checked_at_index("- [/] started", 0, true), "- [x] started");
        assert_eq!(set_task_checked_at_index("- [/] started", 0, false), "- [/] started");
        assert_eq!(set_task_checked_at_index("- [x] a", 0, false), "- [ ] a");
        assert_eq!(set_task_checked_at_index("- [-] a", 0, false), "- [ ] a");
        assert_eq!(set_task_checked_at_index("- [>] a", 0, true), "- [x] a");
    }

    #[test]
    fn index_walks_keep_crlf_endings() {
        let md = "# Saved on Windows\r\n\r\n- [ ] one\r\n- [ ] two\r\n";
        assert_eq!(set_task_checked_at_index(md, 1, true), "# Saved on Windows\r\n\r\n- [ ] one\r\n- [x] two\r\n");
        let md = "- [ ] a\n- [ ] b\r\n- [ ] c\n";
        assert_eq!(set_task_checked_at_index(md, 1, true), "- [ ] a\n- [x] b\r\n- [ ] c\n");
        assert_eq!(set_task_due_at_index("- [ ] a\r\n", 0, Some("2026-01-02")), "- [ ] a due:2026-01-02\r\n");
    }

    #[test]
    fn task_line_shapes() {
        for (line, expected) in [
            ("* [ ] star", "* [x] star"),
            ("+ [ ] plus", "+ [x] plus"),
            ("12) [ ] paren", "12) [x] paren"),
            ("> > - [ ] quoted", "> > - [x] quoted"),
            ("\t- [ ] tab", "\t- [x] tab"),
            ("\u{A0}- [ ] nbsp", "\u{A0}- [x] nbsp"),
            ("-\u{3000}[ ] ideographic space", "-\u{3000}[x] ideographic space"),
            ("- [ ]", "- [x]"),
        ] {
            assert_eq!(set_task_checked_at_index(line, 0, true), expected, "{line:?}");
        }
        for line in ["-[ ] no space", "- [ ]x\u{2028}y", "- [y] state", "a - [ ] mid", "- [ ] cr\rinside"] {
            assert_eq!(set_task_checked_at_index(line, 0, true), line, "{line:?}");
        }
    }

    #[test]
    fn waiting() {
        assert_eq!(set_task_waiting_at_index("- [ ] a", 0, true), "- [ ] a @waiting");
        assert_eq!(set_task_waiting_at_index("- [ ] a  \t", 0, true), "- [ ] a @waiting");
        assert_eq!(set_task_waiting_at_index("- [ ]", 0, true), "- [ ] @waiting");
        assert_eq!(set_task_waiting_at_index("- [ ] a @waiting", 0, true), "- [ ] a @waiting");
        assert_eq!(set_task_waiting_at_index("- [ ] a @Waiting", 0, true), "- [ ] a @Waiting");
        assert_eq!(set_task_waiting_at_index("- [ ] a @waiting", 0, false), "- [ ] a");
        assert_eq!(
            set_task_waiting_at_index("- [ ] a @waiting due:2026-04-30", 0, false),
            "- [ ] a due:2026-04-30"
        );
        assert_eq!(set_task_waiting_at_index("- [ ] a", 0, false), "- [ ] a");
        assert_eq!(set_task_waiting_at_index("- [ ] a\n- [ ] b", 1, true), "- [ ] a\n- [ ] b @waiting");
        // `@waitingroom` is not the marker; only the first marker goes.
        assert_eq!(set_task_waiting_at_index("- [ ] a @waitingroom", 0, true), "- [ ] a @waitingroom @waiting");
        assert_eq!(set_task_waiting_at_index("- [ ] a @waiting x @waiting", 0, false), "- [ ] a x @waiting");
        // Removal collapses whitespace runs but leaves a lone tab.
        assert_eq!(set_task_waiting_at_index("  - [x] a\tb  @waiting  c", 0, false), "  - [x] a\tb c");
    }

    #[test]
    fn priority() {
        assert_eq!(set_task_priority_at_index("- [ ] a", 0, Some("high")), "- [ ] a !high");
        assert_eq!(set_task_priority_at_index("- [ ] a !med", 0, Some("high")), "- [ ] a !high");
        assert_eq!(set_task_priority_at_index("- [ ] !low a", 0, Some("high")), "- [ ] a !high");
        assert_eq!(set_task_priority_at_index("- [ ] a !high", 0, None), "- [ ] a");
        assert_eq!(set_task_priority_at_index("- [ ] a !high", 0, Some("")), "- [ ] a");
        assert_eq!(set_task_priority_at_index("- [ ] a !high due:2026-04-30", 0, None), "- [ ] a due:2026-04-30");
        assert_eq!(set_task_priority_at_index("- [ ] a !h", 0, Some("low")), "- [ ] a !low");
        assert_eq!(set_task_priority_at_index("- [ ] a !MEDIUM", 0, Some("low")), "- [ ] a !low");
        assert_eq!(set_task_priority_at_index("- [ ] !medical record", 0, Some("high")), "- [ ] !medical record !high");
        assert_eq!(set_task_priority_at_index("- [ ] a !high\n- [ ] b", 1, Some("med")), "- [ ] a !high\n- [ ] b !med");
        assert_eq!(set_task_priority_at_index("    - [/] nested !low", 0, Some("med")), "    - [/] nested !med");
        // Only the first token is removed (non-global regex).
        assert_eq!(set_task_priority_at_index("- [ ] a !high b !low", 0, None), "- [ ] a b !low");
    }

    #[test]
    fn due() {
        assert_eq!(set_task_due_at_index("- [ ] a", 0, Some("2026-04-30")), "- [ ] a due:2026-04-30");
        assert_eq!(set_task_due_at_index("- [ ] a due:2026-04-29", 0, Some("2026-04-30")), "- [ ] a due:2026-04-30");
        assert_eq!(set_task_due_at_index("- [ ] a due:tomorrow", 0, Some("2026-04-30")), "- [ ] a due:2026-04-30");
        assert_eq!(set_task_due_at_index("- [ ] a due:2026-04-30 !high", 0, None), "- [ ] a !high");
        assert_eq!(set_task_due_at_index("- [ ] a due: 2026-04-30 !high", 0, Some("")), "- [ ] a !high");
        assert_eq!(set_task_due_at_index("- [ ] a DUE:2026-04-30 b", 0, Some("2026-05-01")), "- [ ] a b due:2026-05-01");
        assert_eq!(set_task_due_at_index("  - [x] nested", 0, Some("2026-05-01")), "  - [x] nested due:2026-05-01");
        // `overdue:` is not a due token: `due` must follow the start or a space.
        assert_eq!(set_task_due_at_index("- [ ] overdue:x", 0, Some("2026-05-01")), "- [ ] overdue:x due:2026-05-01");
        assert_eq!(set_task_due_at_index("- [ ] a", 3, Some("2026-05-01")), "- [ ] a");
    }

    #[test]
    fn field() {
        assert_eq!(set_task_field_at_index("- [ ] ship it", 0, "status", Some("review")), "- [ ] ship it @status:review");
        assert_eq!(
            set_task_field_at_index("- [ ] ship it @status:backlog", 0, "status", Some("review")),
            "- [ ] ship it @status:review"
        );
        assert_eq!(set_task_field_at_index("- [ ] ship it @status:review", 0, "status", None), "- [ ] ship it");
        assert_eq!(set_task_field_at_index("- [ ] a @status:review due:2026-04-30", 0, "status", Some("")), "- [ ] a due:2026-04-30");
        assert_eq!(
            set_task_field_at_index("- [ ] a !high due:2026-04-30", 0, "status", Some("in_progress")),
            "- [ ] a !high due:2026-04-30 @status:in_progress"
        );
        assert_eq!(
            set_task_field_at_index("- [ ] a\n- [ ] b\n- [ ] c", 1, "status", Some("done")),
            "- [ ] a\n- [ ] b @status:done\n- [ ] c"
        );
        assert_eq!(set_task_field_at_index("- [ ] a", 0, "sprint", Some("24")), "- [ ] a @sprint:24");
        assert_eq!(
            set_task_field_at_index("- [ ] a @status:review @sprint:24", 0, "sprint", Some("25")),
            "- [ ] a @status:review @sprint:25"
        );
        assert_eq!(set_task_field_at_index("- [ ] a @sprint:24 @status:done", 0, "sprint", None), "- [ ] a @status:done");
        for bad in ["Bad Key", "Status", "1x", "", "a.b"] {
            assert_eq!(set_task_field_at_index("- [ ] a", 0, bad, Some("x")), "- [ ] a", "{bad:?}");
        }
        // Case-insensitive on ASCII only, as a JS `i` regex without `u` is.
        assert_eq!(set_task_field_at_index("- [ ] a @STATUS:x", 0, "status", Some("y")), "- [ ] a @status:y");
        assert_eq!(
            set_task_field_at_index("- [ ] a @\u{17F}tatus:x", 0, "status", Some("y")),
            "- [ ] a @\u{17F}tatus:x @status:y"
        );
        // `\S+` takes any non-space value, not only a slug.
        assert_eq!(set_task_field_at_index("- [ ] a @status:x,y! b", 0, "status", None), "- [ ] a b");
    }

    #[test]
    fn text() {
        assert_eq!(set_task_text_at_index("- [x] old text", 0, "new text"), "- [x] new text");
        assert_eq!(set_task_text_at_index("  - [ ] a\n  - [ ] b", 1, "edited"), "  - [ ] a\n  - [ ] edited");
        assert_eq!(
            set_task_text_at_index("- [ ] a", 0, "do thing due:2026-07-01 !high"),
            "- [ ] do thing due:2026-07-01 !high"
        );
        assert_eq!(set_task_text_at_index("- [ ] something", 0, "   "), "- [ ]");
        assert_eq!(set_task_text_at_index("- [ ] a", 0, "\u{FEFF} b \u{A0}"), "- [ ] b");
        assert_eq!(set_task_text_at_index("> 3. [/] q", 0, "r"), "> 3. [/] r");
        assert_eq!(set_task_text_at_index("- [ ] same", 0, "same"), "- [ ] same");
    }

    #[test]
    fn in_progress_and_cancelled() {
        assert_eq!(set_task_in_progress_at_index("- [ ] Task 1", 0, true), "- [/] Task 1");
        assert_eq!(set_task_in_progress_at_index("- [x] Task 1", 0, true), "- [/] Task 1");
        assert_eq!(set_task_in_progress_at_index("- [/] Task 1", 0, false), "- [ ] Task 1");
        let line = "- [ ] Ship it due:2026-08-04 !high #work [[Spec]]";
        let started = set_task_in_progress_at_index(line, 0, true);
        assert_eq!(started, "- [/] Ship it due:2026-08-04 !high #work [[Spec]]");
        assert_eq!(set_task_in_progress_at_index(&started, 0, false), line);
        assert_eq!(set_task_cancelled_at_index("- [ ] Task 1", 0, true), "- [-] Task 1");
        assert_eq!(set_task_cancelled_at_index("- [x] Task 1", 0, true), "- [-] Task 1");
        assert_eq!(set_task_cancelled_at_index("- [-] Task 1", 0, false), "- [ ] Task 1");
        assert_eq!(set_task_cancelled_at_index("1. [ ] a\n   - [ ] b", 1, true), "1. [ ] a\n   - [-] b");
    }

    /// updateTaskLine's sequence of setters on a single line, as upstream's
    /// asset-work-tools test drives update_task.
    #[test]
    fn update_task_line_sequence() {
        let mut line = "- [ ] one due:2026-01-01 !low #tag".to_string();
        line = set_task_due_at_index(&line, 0, Some("2026-02-02"));
        assert_eq!(line, "- [ ] one !low #tag due:2026-02-02");
        line = set_task_priority_at_index(&line, 0, None);
        line = set_task_waiting_at_index(&line, 0, true);
        line = set_task_field_at_index(&line, 0, "status", Some("doing"));
        assert_eq!(line, "- [ ] one #tag due:2026-02-02 @waiting @status:doing");
        let open = |l: &str| set_task_in_progress_at_index(&set_task_checked_at_index(l, 0, false), 0, false);
        assert_eq!(open("- [x] two"), "- [ ] two");
        assert_eq!(open("- [/] two"), "- [ ] two");
        assert_eq!(open("- [-] two"), "- [ ] two");
        assert_eq!(set_task_text_at_index("- [ ] two", 0, "renamed !high"), "- [ ] renamed !high");
    }

    /* ---------- frontmatter --------------------------------------------- */

    #[test]
    fn yaml_value_quoting() {
        for v in ["hello world", "2026-08-07", "-5", "a, b", "x [y]", "v2.25.0", "-x", "?x", "caf\u{E9}"] {
            assert_eq!(yaml_value(v), v);
        }
        for v in [
            "[[Project X]]", "[a]", "{x}", "- item", "-", "? key", "> folded", "|literal", "&anchor", "*alias",
            "!tag", "%directive", "`tick", ",lead", "] stray", "a: b", "#tag", " padded", "padded ", "", "-\u{A0}x",
            "it's", "say \"hi\"", "two\nlines", "\u{FEFF}bom", "@mention",
        ] {
            assert_eq!(yaml_value(v), serde_json::to_string(v).unwrap(), "{v:?}");
        }
        // JSON.stringify's escapes.
        assert_eq!(yaml_value("a\u{1}\u{1F}\u{7F}\u{2028}\"\\\u{8}\u{C}\n\r\t"), "\"a\\u0001\\u001f\u{7F}\u{2028}\\\"\\\\\\b\\f\\n\\r\\t\"");
    }

    #[test]
    fn update_frontmatter_add_replace_delete() {
        let body = "---\ntags: [task]\nstatus: open\n---\n# Do it\n";
        assert_eq!(
            update_frontmatter_fields(body, &[("due", Some("2026-02-02"))]),
            "---\ntags: [task]\nstatus: open\ndue: 2026-02-02\n---\n# Do it\n"
        );
        assert_eq!(
            update_frontmatter_fields(body, &[("Status", Some("done"))]),
            "---\ntags: [task]\nStatus: done\n---\n# Do it\n"
        );
        assert_eq!(update_frontmatter_fields(body, &[("status", None)]), "---\ntags: [task]\n---\n# Do it\n");
        assert_eq!(update_frontmatter_fields(body, &[("missing", None)]), body);
        assert_eq!(update_frontmatter_fields(body, &[]), body);
        // Block lists and everything else stay byte for byte; an indented key
        // line is a key line too, and is rewritten unindented.
        let body = "---\ntags:\n  - task\n  status: nested\ntitle: x\n---\nbody";
        assert_eq!(
            update_frontmatter_fields(body, &[("status", Some("done")), ("tags", None)]),
            "---\n  - task\nstatus: done\ntitle: x\n---\nbody"
        );
    }

    #[test]
    fn update_frontmatter_without_a_block() {
        assert_eq!(
            update_frontmatter_fields("Body\n", &[("project", Some("[[Project X]]")), ("gone", None)]),
            "---\nproject: \"[[Project X]]\"\n---\nBody\n"
        );
        assert_eq!(update_frontmatter_fields("Body\r\n", &[("gone", None)]), "Body\r\n");
        assert_eq!(update_frontmatter_fields("a\r\nb", &[("k", Some("v"))]), "---\nk: v\n---\na\nb");
        assert_eq!(update_frontmatter_fields("", &[("k", Some(""))]), "---\nk: \"\"\n---\n");
        // `---\n---\n` is not a block (it needs a line between), and an
        // unclosed one is not either.
        assert_eq!(update_frontmatter_fields("---\n---\nx", &[("k", Some("v"))]), "---\nk: v\n---\n---\n---\nx");
        assert_eq!(update_frontmatter_fields("---\nk: v\n", &[("k", Some("w"))]), "---\nk: w\n---\n---\nk: v\n");
    }

    #[test]
    fn update_frontmatter_crlf_and_edges() {
        let body = "---\r\nstatus: open\r\n---\r\nline\r\n";
        assert_eq!(update_frontmatter_fields(body, &[("status", Some("done"))]), "---\nstatus: done\n---\nline\n");
        // A closing `---` need not end its line, and no newline after it is fine.
        assert_eq!(update_frontmatter_fields("---\na: 1\n----x", &[("a", Some("2"))]), "---\na: 2\n---\n-x");
        assert_eq!(update_frontmatter_fields("---\na: 1\n---", &[("b", Some("2"))]), "---\na: 1\nb: 2\n---\n");
        // Values that need quoting get it.
        assert_eq!(
            update_frontmatter_fields("---\na: 1\n---\n", &[("title", Some("a: b")), ("x", Some("plain"))]),
            "---\na: 1\ntitle: \"a: b\"\nx: plain\n---\n"
        );
    }

    #[test]
    fn update_frontmatter_key_order_is_object_order() {
        // A key repeated in different case keeps its place and takes the later
        // spelling and value (Map.set); new keys follow in object order.
        assert_eq!(
            update_frontmatter_fields("x", &[("b", Some("1")), ("a", Some("2")), ("B", Some("3"))]),
            "---\nB: 3\na: 2\n---\nx"
        );
        // An exact repeat collapses first, as an object literal does.
        assert_eq!(
            update_frontmatter_fields("x", &[("a", Some("1")), ("A", Some("2")), ("a", Some("3"))]),
            "---\nA: 2\n---\nx"
        );
        // Array-index keys enumerate first, ascending.
        assert_eq!(
            update_frontmatter_fields("x", &[("z", Some("1")), ("10", Some("2")), ("2", Some("3")), ("01", Some("4"))]),
            "---\n2: 3\n10: 2\nz: 1\n01: 4\n---\nx"
        );
    }

    #[test]
    fn task_file_states() {
        let body = "---\ntags: [task]\nstatus: open\n---\n# Do it\n";
        let done = set_task_file_status(body, true, "2026-11-14");
        assert_eq!(done, "---\ntags: [task]\nstatus: done\ncompletedDate: 2026-11-14\n---\n# Do it\n");
        assert_eq!(set_task_file_status(&done, false, "2026-11-14"), body);
        assert_eq!(
            set_task_file_cancelled(&done, true),
            "---\ntags: [task]\nstatus: cancelled\n---\n# Do it\n"
        );
        assert_eq!(set_task_file_cancelled("---\nstatus: cancelled\n---\n", false), "---\nstatus: open\n---\n");
        let started = set_task_file_in_progress("---\ntags: [task]\nstatus: open\n---\n", true);
        assert_eq!(started, "---\ntags: [task]\nstatus: in-progress\n---\n");
        assert_eq!(set_task_file_in_progress(&started, false), "---\ntags: [task]\nstatus: open\n---\n");
        // No block yet: one is made from the non-null fields.
        assert_eq!(set_task_file_status("# T\n", true, "2026-11-14"), "---\nstatus: done\ncompletedDate: 2026-11-14\n---\n# T\n");
        assert_eq!(set_task_file_in_progress("# T\n", false), "---\nstatus: open\n---\n# T\n");
    }

    /// updateFileTask's path for `{ due, priority: 'med', state: 'done' }`.
    #[test]
    fn update_file_task_sequence() {
        let mut body = "---\ntags: [task]\nstatus: open\n---\n# Do it\n".to_string();
        body = set_task_file_status(&body, true, "2026-11-14");
        body = update_frontmatter_fields(&body, &[("due", Some("2026-02-02"))]);
        body = update_frontmatter_fields(&body, &[("priority", Some("normal"))]);
        assert_eq!(
            body,
            "---\ntags: [task]\nstatus: done\ncompletedDate: 2026-11-14\ndue: 2026-02-02\npriority: normal\n---\n# Do it\n"
        );
        body = update_frontmatter_fields(&body, &[("status", Some("waiting")), ("completedDate", None)]);
        assert_eq!(body, "---\ntags: [task]\nstatus: waiting\ndue: 2026-02-02\npriority: normal\n---\n# Do it\n");
    }

    #[test]
    fn collapse_and_trim_helpers() {
        assert_eq!(collapse_ws("a  b\tc \t d\u{A0}\u{A0}e"), "a b\tc d e");
        assert_eq!(tidy("a  b\t\tc \u{A0} d  \u{FEFF}"), "a b c \u{A0} d");
        assert_eq!(js_trim_end("x \u{2028}\u{FEFF}"), "x");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
        assert_eq!(ascii_ci("due_1-x"), "[dD][uU][eE]_1-[xX]");
    }
}
