//! A note's sections, for the MCP `note_sections` / `edit_section` tools:
//! reading and editing part of a note by its heading instead of by line
//! numbers or the whole body. Port of upstream
//! apps/desktop/src/mcp/note-sections.ts in full, plus what it imports:
//! - `parseOutline` and `ATX_RE` from packages/app-core/src/lib/outline.ts;
//! - `scanMarkdownLines`, `frontmatterEndIndex`, `isFrontmatterFence`,
//!   `FENCE_OPEN_RE` and `FENCE_CLOSE_RE` from
//!   packages/shared-domain/src/markdown-lines.ts;
//! - `goQuote` from packages/shared-domain/src/mcp-scope.ts, as the private
//!   [`go_quote`], for the error messages.
//!
//! Left out because nothing here reads them: an outline item's `from` (the
//! UTF-16 offset of its line), `scanMarkdownLines`' `includeFrontmatter`
//! option, and outline.ts' `activeOutlineLineForCursor`.
//!
//! Every regex is a JS regex re-spelled for Rust, by the rules of the `//!`
//! header of tasks.rs: JS `\s` and `trim` are ECMAScript WhiteSpace plus
//! LineTerminator (`ws!` / `is_js_ws`), JS `\d` is `[0-9]`, and JS `.` stops
//! at `\r`, U+2028 and U+2029 as well as `\n` (`dot!`). Line numbers count
//! `\n`-separated lines, which JS and Rust split identically, and
//! `toLowerCase` is `str::to_lowercase` (full mapping with Final_Sigma in
//! both; only a Unicode-version difference could tell them apart).

use std::sync::LazyLock;

use fancy_regex::Regex;
use serde::Serialize;

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

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static section regex")
}

/* ---------- markdown-lines.ts ----------------------------------------- */

/// `FENCE_OPEN_RE = /^\s*(`{3,}|~{3,})(.*)\r?$/`: group 1 the marker, group 2
/// the info string.
static FENCE_OPEN_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^[", ws!(), "]*(`{3,}|~{3,})(", dot!(), "*)\r?$")));
/// `FENCE_CLOSE_RE = /^\s*(`{3,}|~{3,})[ \t]*\r?$/`.
static FENCE_CLOSE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^[", ws!(), "]*(`{3,}|~{3,})[ \t]*\r?$")));

/// `isFrontmatterFence`: `---` alone on its line.
fn is_frontmatter_fence(line: &str) -> bool {
    js_trim(line) == "---"
}

/// `frontmatterEndIndex`: the line of the closing `---` of a leading YAML
/// block, or `None` (upstream's -1).
fn frontmatter_end_index(lines: &[&str]) -> Option<usize> {
    if lines.len() < 2 || !is_frontmatter_fence(lines[0]) {
        return None;
    }
    (1..lines.len()).find(|&i| is_frontmatter_fence(lines[i]))
}

/// Upstream `MarkdownLine`, less `from`.
struct MarkdownLine<'a> {
    text: &'a str,
    /// The line after it, whatever it is (setext lookahead).
    next: Option<&'a str>,
    /// 1-based.
    line: usize,
}

/// `scanMarkdownLines`: the lines of a body that markdown owns, skipping a
/// leading frontmatter block and the inside of fenced code blocks.
fn scan_markdown_lines(body: &str) -> Vec<MarkdownLine<'_>> {
    let mut scanned = Vec::new();
    if body.is_empty() {
        return scanned;
    }
    let lines: Vec<&str> = body.split('\n').collect();
    let frontmatter_end = frontmatter_end_index(&lines);
    // The marker run that opened the current fence, if inside one.
    let mut fence: Option<&str> = None;

    for (i, &raw) in lines.iter().enumerate() {
        if frontmatter_end.is_some_and(|end| i <= end) {
            continue;
        }

        if let Some(open_marker) = fence {
            if let Ok(Some(close)) = FENCE_CLOSE_RE.captures(raw) {
                let marker = close.get(1).map_or("", |m| m.as_str());
                if marker.as_bytes().first() == open_marker.as_bytes().first()
                    && marker.len() >= open_marker.len()
                {
                    fence = None;
                }
            }
            continue;
        }

        if let Ok(Some(open)) = FENCE_OPEN_RE.captures(raw) {
            let marker = open.get(1).map_or("", |m| m.as_str());
            let info = open.get(2).map_or("", |m| m.as_str());
            // A backtick fence's info string may not hold a backtick; when it
            // does, the line is an inline code span, not a fence.
            if !marker.starts_with('`') || !info.contains('`') {
                fence = Some(marker);
                continue;
            }
        }

        scanned.push(MarkdownLine { text: raw, next: lines.get(i + 1).copied(), line: i + 1 });
    }

    scanned
}

/* ---------- outline.ts ------------------------------------------------- */

/// `ATX_RE = /^(#{1,6})\s+(.+?)\s*#*\s*$/`.
static ATX_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        "^(#{1,6})[",
        ws!(),
        "]+(",
        dot!(),
        "+?)[",
        ws!(),
        "]*#*[",
        ws!(),
        "]*$"
    ))
});
/// `SETEXT_UNDERLINE_RE = /^(=+|-{2,})\s*$/`.
static SETEXT_UNDERLINE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^(=+|-{2,})[", ws!(), "]*$")));

/// Upstream `OutlineItem`, less `from`.
struct OutlineItem {
    /// 1..6
    level: usize,
    text: String,
    /// 1-based.
    line: usize,
}

/// `parseOutline`: ATX headings, and setext headings (`=` level 1, two or
/// more `-` level 2), outside frontmatter and fenced code.
fn parse_outline(body: &str) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for MarkdownLine { text: raw, next, line } in scan_markdown_lines(body) {
        if let Ok(Some(atx)) = ATX_RE.captures(raw) {
            items.push(OutlineItem {
                level: atx.get(1).map_or(0, |m| m.as_str().len()),
                text: js_trim(atx.get(2).map_or("", |m| m.as_str())).to_string(),
                line,
            });
            continue;
        }
        if let Some(next) = next {
            if !js_trim(raw).is_empty() {
                if let Ok(Some(under)) = SETEXT_UNDERLINE_RE.captures(next) {
                    let level = if under.get(1).is_some_and(|m| m.as_str().starts_with('=')) { 1 } else { 2 };
                    items.push(OutlineItem { level, text: js_trim(raw).to_string(), line });
                }
            }
        }
    }
    items
}

/* ---------- mcp-scope.ts ----------------------------------------------- */

/// `goQuote`: Go's `%q` for the strings a tool argument can hold, so a refusal
/// reads byte for byte the same from either engine: printable text as is,
/// ASCII controls escaped the way strconv.Quote escapes them.
fn go_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0B}' => out.push_str("\\v"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/* ---------- note-sections.ts ------------------------------------------ */

/// A heading and what sits under it, up to the next heading of the same or a
/// higher level. Lines are zero-based, as in insert_at_line and a task's
/// lineNumber; `end_line_number` is exclusive. Serializes as upstream's
/// `{ level, text, lineNumber, endLineNumber }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSection {
    pub level: usize,
    pub text: String,
    pub line_number: usize,
    pub end_line_number: usize,
}

/// A [`NoteSection`] with where its text starts and stops (upstream's
/// internal `SectionExtent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionExtent {
    pub level: usize,
    pub text: String,
    pub line_number: usize,
    pub end_line_number: usize,
    /// The first line after the heading (a setext heading has two).
    pub body_start: usize,
    /// Where its own text stops: its first subheading, else the section's end.
    pub own_end: usize,
}

/// Upstream `SectionEditMode`: `'append' | 'prepend' | 'replace'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEditMode {
    Append,
    Prepend,
    Replace,
}

/// `SECTION_LIST_ITEM_RE = /^\s*(?:[-+*]|\d+[.)])\s/`.
static SECTION_LIST_ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| re(concat!("^[", ws!(), "]*(?:[-+*]|[0-9]+[.)])[", ws!(), "]")));

fn is_list_item(line: &str) -> bool {
    SECTION_LIST_ITEM_RE.is_match(line).unwrap_or(false)
}

/// `usesCrlf`: whether every line break of a note is CRLF, so an edit made in
/// LF can be put back. A note with mixed endings comes out in LF.
pub fn uses_crlf(body: &str) -> bool {
    if !body.contains("\r\n") {
        return false;
    }
    // `!/(^|[^\r])\n/.test(body)`
    let b = body.as_bytes();
    !b.iter().enumerate().any(|(i, &c)| c == b'\n' && (i == 0 || b[i - 1] != b'\r'))
}

/// `splitNoteLines`: `body.replace(/\r\n/g, '\n').split('\n')`, borrowed from
/// `body`. Splitting at `\n` and dropping one `\r` from the end of every piece
/// but the last (the only one not followed by a `\n`) is the same thing.
fn split_note_lines(body: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = body.split('\n').collect();
    let last = lines.len() - 1;
    for line in &mut lines[..last] {
        *line = line.strip_suffix('\r').unwrap_or(line);
    }
    lines
}

/// `lines.slice(start, end)` with JS's clamping.
fn js_slice<'s, 'a>(lines: &'s [&'a str], start: usize, end: usize) -> &'s [&'a str] {
    let end = end.min(lines.len());
    let start = start.min(end);
    &lines[start..end]
}

fn section_extents(body: &str) -> Vec<SectionExtent> {
    let normalized = body.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let outline = parse_outline(&normalized);
    outline
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let line = item.line - 1;
            let is_atx = lines.get(line).is_some_and(|l| ATX_RE.is_match(l).unwrap_or(false));
            let body_start = if is_atx { line + 1 } else { line + 2 };
            let end = outline[i + 1..]
                .iter()
                .find(|later| later.level <= item.level)
                .map_or(lines.len(), |later| later.line - 1);
            let own_end = match outline.get(i + 1) {
                Some(next) if next.line - 1 < end => next.line - 1,
                _ => end,
            };
            SectionExtent {
                level: item.level,
                text: item.text.clone(),
                line_number: line,
                end_line_number: end,
                body_start,
                own_end,
            }
        })
        .collect()
}

/// `noteSections`: every heading of a note with its extent.
pub fn note_sections(body: &str) -> Vec<NoteSection> {
    section_extents(body)
        .into_iter()
        .map(|s| NoteSection {
            level: s.level,
            text: s.text,
            line_number: s.line_number,
            end_line_number: s.end_line_number,
        })
        .collect()
}

/// `findSection`: the section a heading names, ignoring case and any leading
/// `#`s. `line`, when given, says which one when a heading appears more than
/// once (upstream passes `Math.trunc(lineNumber)`). The error is upstream's
/// message, word for word.
pub fn find_section(body: &str, heading: &str, line: Option<i64>) -> Result<SectionExtent, String> {
    let sections = section_extents(body);
    let want = js_trim(js_trim(heading).trim_start_matches('#')).to_lowercase();
    let matches: Vec<&SectionExtent> = sections
        .iter()
        .filter(|s| {
            s.text.to_lowercase() == want
                && line.is_none_or(|l| i64::try_from(s.line_number).is_ok_and(|n| n == l))
        })
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => {
            let mut listed: Vec<&str> = sections.iter().map(|s| s.text.as_str()).collect();
            if listed.len() > 20 {
                listed.truncate(20);
                listed.push("...");
            }
            if listed.is_empty() {
                return Err(format!("No heading {}: this note has no headings.", go_quote(heading)));
            }
            Err(format!("No heading {}. Headings: {}.", go_quote(heading), listed.join(" | ")))
        }
        _ => Err(format!(
            "Heading {} appears more than once, on lines {}; pass lineNumber to pick one.",
            go_quote(heading),
            matches.iter().map(|s| s.line_number.to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// `trimBlankLines`: the blank lines leading the slice, and what is left
/// between them and the trailing blank lines.
fn trim_blank_lines<'s, 'a>(lines: &'s [&'a str]) -> (usize, &'s [&'a str]) {
    let mut end = lines.len();
    while end > 0 && js_trim(lines[end - 1]).is_empty() {
        end -= 1;
    }
    let mut lead = 0;
    while lead < end && js_trim(lines[lead]).is_empty() {
        lead += 1;
    }
    (lead, &lines[lead..end])
}

/// `sectionBody`: the text under a heading, without the heading or the blank
/// lines around it. Lines are joined with `\n` whatever the note uses.
pub fn section_body(body: &str, section: &SectionExtent) -> String {
    let lines = split_note_lines(body);
    let start = section.body_start.min(section.end_line_number);
    trim_blank_lines(js_slice(&lines, start, section.end_line_number)).1.join("\n")
}

/// `editSectionInBody`: replaces what sits under a heading, subheadings
/// included, or adds text at the start of it or at the end of its own text,
/// above any subheading. The heading and everything else keep their text,
/// one blank line separates the edit from the next heading, and a CRLF note
/// stays CRLF.
pub fn edit_section_in_body(body: &str, section: &SectionExtent, mode: SectionEditMode, text: &str) -> String {
    let lines = split_note_lines(body);
    let start = section.body_start.min(section.end_line_number);
    let end = if mode == SectionEditMode::Append {
        start.max(section.own_end)
    } else {
        section.end_line_number
    };
    let (mut lead, core) = trim_blank_lines(js_slice(&lines, start, end));
    let normalized_text = text.replace("\r\n", "\n");
    let stripped = normalized_text.trim_end_matches('\n');
    let inserted: Vec<&str> = if stripped.is_empty() { Vec::new() } else { stripped.split('\n').collect() };

    let content: Vec<&str> = match mode {
        SectionEditMode::Replace => inserted,
        SectionEditMode::Prepend => {
            let mut content = inserted;
            if !core.is_empty() {
                content.push("");
                content.extend_from_slice(core);
            }
            content
        }
        SectionEditMode::Append => {
            let mut content = core.to_vec();
            // A list grows by one item; anything else gets its own paragraph.
            if let (Some(last), Some(first)) = (core.last(), inserted.first()) {
                if !is_list_item(last) || !is_list_item(first) {
                    content.push("");
                }
            }
            content.extend(inserted);
            content
        }
    };

    let mut out: Vec<&str> = js_slice(&lines, 0, start).to_vec();
    if !content.is_empty() {
        // Keep a heading's blank line as written; an empty section gets one.
        if core.is_empty() || mode == SectionEditMode::Replace {
            lead = 1;
        }
        out.extend(std::iter::repeat_n("", lead));
        out.extend(content);
    }
    // A section that ended the note still ends it with a newline.
    out.push("");
    if end < lines.len() {
        out.extend_from_slice(&lines[end..]);
    }
    let edited = out.join("\n");
    if uses_crlf(body) {
        edited.replace('\n', "\r\n")
    } else {
        edited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline_texts(body: &str) -> Vec<String> {
        parse_outline(body).into_iter().map(|i| i.text).collect()
    }

    fn outline_levels(body: &str) -> Vec<(usize, String)> {
        parse_outline(body).into_iter().map(|i| (i.level, i.text)).collect()
    }

    fn sec(level: usize, text: &str, line_number: usize, end_line_number: usize) -> NoteSection {
        NoteSection { level, text: text.into(), line_number, end_line_number }
    }

    fn edit(body: &str, heading: &str, mode: SectionEditMode, text: &str) -> String {
        let section = find_section(body, heading, None).expect("heading");
        edit_section_in_body(body, &section, mode, text)
    }

    fn edit_at(body: &str, heading: &str, line: i64, mode: SectionEditMode, text: &str) -> String {
        let section = find_section(body, heading, Some(line)).expect("heading");
        edit_section_in_body(body, &section, mode, text)
    }

    use SectionEditMode::{Append, Prepend, Replace};

    /* outline.test.ts -------------------------------------------------- */

    #[test]
    fn keeps_headings_after_a_single_line_triple_backtick_span() {
        let body = "# Section 1\n## Sub 1\n```[[Verweisziel|Beschriftung des Verweises]]```\n# Section 2\n## Sub 2\n# Section 3";
        assert_eq!(outline_texts(body), ["Section 1", "Sub 1", "Section 2", "Sub 2", "Section 3"]);
    }

    #[test]
    fn keeps_headings_after_a_code_block_with_wikilinks() {
        let body = "# Section 1\n## Sub 1\n\n```\n[[Verweisziel|Beschriftung des Verweises]]\n\n[[Linux-Backup]]\n```\n\n# Section 2\n## Sub 2\n# Section 3";
        assert_eq!(outline_texts(body), ["Section 1", "Sub 1", "Section 2", "Sub 2", "Section 3"]);
    }

    #[test]
    fn keeps_headings_after_a_code_block_with_a_fence_like_line() {
        let body = "# Section 1\n```\nshow a fence: ```\n~~~\n```\n# Section 2";
        assert_eq!(outline_texts(body), ["Section 1", "Section 2"]);
    }

    #[test]
    fn keeps_headings_after_a_language_tagged_block() {
        assert_eq!(outline_texts("# Section 1\n```markdown\n# not a heading\n```\n# Section 2"), ["Section 1", "Section 2"]);
    }

    #[test]
    fn skips_headings_inside_a_fence() {
        assert_eq!(outline_texts("# Real\n```\n# fake heading in code\n```\n# Also real"), ["Real", "Also real"]);
    }

    #[test]
    fn skips_headings_inside_a_fence_with_crlf() {
        assert_eq!(outline_texts("# Real\r\n```sh\r\n# fake heading in code\r\n```\r\n# Also real\r\n"), ["Real", "Also real"]);
    }

    #[test]
    fn is_unaffected_by_a_single_backtick_span() {
        assert_eq!(outline_texts("# Section 1\n`[[Linux-Backup]]`\n# Section 2"), ["Section 1", "Section 2"]);
    }

    #[test]
    fn frontmatter_closing_fence_is_not_a_setext_underline() {
        assert!(parse_outline("---\ntitle: Test\ntype: Test\n---").is_empty());
        let items = parse_outline("---\ntitle: Test\ntype: Test\n---\n\n# Real Heading");
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].level, items[0].text.as_str(), items[0].line), (1, "Real Heading", 6));
        assert_eq!(outline_levels("---\ntitle: Test\n---\n\nA Heading\n---\n\nbody"), [(2, "A Heading".into())]);
        assert_eq!(
            outline_levels("Title\n===\n\ntext\n\n---\n\n# After Rule"),
            [(1, "Title".into()), (1, "After Rule".into())]
        );
        assert_eq!(outline_texts("---\n---\n# Only Heading"), ["Only Heading"]);
    }

    #[test]
    fn a_lone_dash_under_a_line_is_a_list() {
        assert!(parse_outline("Some text\n-\n").is_empty());
        assert!(parse_outline("Some text\n- \n").is_empty());
        let items = parse_outline("Some text\n--\n");
        assert_eq!((items[0].level, items[0].text.as_str(), items[0].line), (2, "Some text", 1));
    }

    #[test]
    fn atx_closing_hashes_and_spacing() {
        assert_eq!(
            outline_levels("# One #\n##   Two   ##  \n###### Six\n####### Seven\n#NoSpace\n  # Indented\n#\tTab"),
            [(1, "One".into()), (2, "Two".into()), (6, "Six".into()), (1, "Tab".into())]
        );
        // The JS `\s` class includes U+FEFF and U+3000.
        assert_eq!(outline_texts("#\u{3000}Wide\u{FEFF}"), ["Wide"]);
        // ...and JS `.` stops at U+2028, so such a line is no heading.
        assert!(parse_outline("# a\u{2028}b").is_empty());
        // A fence opener whose info string runs into U+2028 does not open.
        assert_eq!(outline_texts("```a\u{2028}b\n# H"), ["H"]);
    }

    #[test]
    fn fences_need_their_own_marker_to_close() {
        assert_eq!(outline_texts("````\n```\n# in\n````\n# out"), ["out"]);
        assert_eq!(outline_texts("~~~\n```\n# in\n~~~\n# out"), ["out"]);
        // A closer carries no info string.
        assert_eq!(outline_texts("```\n``` js\n# in\n```\n# out"), ["out"]);
        // An unclosed fence hides everything after it.
        assert!(parse_outline("```\n# in").is_empty());
    }

    /* note-sections (asset-work-tools.test.ts / asset-work-review.test.ts) */

    const BODY: &str = "---\ntitle: x\n---\n# N\n\nIntro\n\n## Actions\n\n- [ ] one\n\n### Detail\n\nfine print\n\n```\n# not a heading\n```\n\nSetext\n---\n\nunder\n";

    #[test]
    fn lists_headings_with_zero_based_lines() {
        assert_eq!(
            note_sections(BODY),
            [sec(1, "N", 3, 24), sec(2, "Actions", 7, 19), sec(3, "Detail", 11, 19), sec(2, "Setext", 19, 24)]
        );
        assert_eq!(
            serde_json::to_string(&note_sections(BODY)).unwrap(),
            r#"[{"level":1,"text":"N","lineNumber":3,"endLineNumber":24},{"level":2,"text":"Actions","lineNumber":7,"endLineNumber":19},{"level":3,"text":"Detail","lineNumber":11,"endLineNumber":19},{"level":2,"text":"Setext","lineNumber":19,"endLineNumber":24}]"#
        );
    }

    #[test]
    fn reads_the_part_under_one_heading() {
        let actions = find_section(BODY, "## actions", None).unwrap();
        assert_eq!((actions.text.as_str(), actions.level, actions.line_number, actions.end_line_number), ("Actions", 2, 7, 19));
        assert_eq!(section_body(BODY, &actions), "- [ ] one\n\n### Detail\n\nfine print\n\n```\n# not a heading\n```");
        let setext = find_section(BODY, "Setext", None).unwrap();
        assert_eq!(setext.body_start, 21);
        assert_eq!(section_body(BODY, &setext), "under");
    }

    #[test]
    fn append_grows_a_list_by_one_item_above_any_subheading() {
        assert_eq!(edit(BODY, "Actions", Append, "- [ ] two"), BODY.replace("- [ ] one\n", "- [ ] one\n- [ ] two\n"));
    }

    #[test]
    fn anything_else_gets_its_own_paragraph_and_replace_can_clear() {
        let body = "# N\n\nText\n\n## B\n\nOld\n\n### C\n\nDeep\n\n## D\n\nEnd\n";
        let body = edit(body, "N", Prepend, "First");
        let body = edit(&body, "B", Replace, "New");
        let body = edit(&body, "D", Append, "More");
        assert_eq!(body, "# N\n\nFirst\n\nText\n\n## B\n\nNew\n\n## D\n\nEnd\n\nMore\n");
        assert_eq!(edit(&body, "B", Replace, ""), "# N\n\nFirst\n\nText\n\n## B\n\n## D\n\nEnd\n\nMore\n");
    }

    #[test]
    fn picks_by_line_when_a_heading_repeats_and_says_what_it_can() {
        let body = "# N\n\n## A\n\n## A\n";
        assert_eq!(
            find_section(body, "A", None),
            Err("Heading \"A\" appears more than once, on lines 2, 4; pass lineNumber to pick one.".into())
        );
        assert_eq!(edit_at(body, "A", 4, Append, "x"), "# N\n\n## A\n\n## A\n\nx\n");
        assert_eq!(edit_at(body, "A", 2, Append, "x"), "# N\n\n## A\n\nx\n\n## A\n");
        assert_eq!(find_section(body, "Nope", None), Err("No heading \"Nope\". Headings: N | A | A.".into()));
        // A line that holds no such heading lists them all.
        assert_eq!(find_section(body, "A", Some(3)), Err("No heading \"A\". Headings: N | A | A.".into()));
        assert_eq!(find_section(body, "A", Some(-2)), Err("No heading \"A\". Headings: N | A | A.".into()));
        assert_eq!(
            find_section("no headings\n", "x", None),
            Err("No heading \"x\": this note has no headings.".into())
        );
        assert_eq!(find_section("", "x", None), Err("No heading \"x\": this note has no headings.".into()));
    }

    #[test]
    fn a_heading_with_nothing_after_its_hashes_reads_the_line_under_it() {
        let body = "#   \nfirst\nsecond\n## Next\nmore\n";
        assert_eq!(note_sections(body).iter().map(|s| s.line_number).collect::<Vec<_>>(), [0, 3]);
        assert_eq!(section_body(body, &find_section(body, "#", None).unwrap()).split('\n').next(), Some("first"));
    }

    #[test]
    fn edit_section_keeps_a_windows_note_a_windows_note() {
        let body = "# Plan\r\n\r\n## A\r\n\r\none\r\n\r\n## B\r\n\r\ntwo\r\n";
        assert!(uses_crlf(body));
        assert_eq!(edit(body, "A", Append, "more"), "# Plan\r\n\r\n## A\r\n\r\none\r\n\r\nmore\r\n\r\n## B\r\n\r\ntwo\r\n");
        // Text sent with CRLF goes in as lines too.
        assert_eq!(edit(body, "B", Replace, "x\r\ny\r\n"), "# Plan\r\n\r\n## A\r\n\r\none\r\n\r\n## B\r\n\r\nx\r\ny\r\n");
        let section = find_section(body, "a", None).unwrap();
        assert_eq!(section_body(body, &section), "one");
    }

    /* own cases ------------------------------------------------------- */

    #[test]
    fn crlf_detection() {
        assert!(!uses_crlf(""));
        assert!(!uses_crlf("a\nb"));
        assert!(uses_crlf("a\r\nb"));
        assert!(!uses_crlf("a\r\nb\nc"));
        assert!(!uses_crlf("\na\r\n"));
        assert!(!uses_crlf("a\r\n\n"));
        assert!(uses_crlf("a\r\n\r\n"));
        // A mixed note comes out in LF.
        assert_eq!(edit("# A\r\none\n", "A", Append, "two"), "# A\none\n\ntwo\n");
    }

    #[test]
    fn setext_headings_have_two_heading_lines() {
        let body = "Title\n=====\nintro\n\nSub\n---\n\ntext\n";
        assert_eq!(note_sections(body), [sec(1, "Title", 0, 9), sec(2, "Sub", 4, 9)]);
        let title = find_section(body, "title", None).unwrap();
        assert_eq!((title.body_start, title.own_end), (2, 4));
        assert_eq!(edit(body, "Title", Append, "more"), "Title\n=====\nintro\n\nmore\n\nSub\n---\n\ntext\n");
        assert_eq!(edit(body, "Sub", Prepend, "first"), "Title\n=====\nintro\n\nSub\n---\n\nfirst\n\ntext\n");
    }

    #[test]
    fn stacked_setext_underlines_clamp_the_body_start() {
        // `===` over `===` makes the first underline a heading too, so the
        // first section ends before its body would start.
        let body = "A\n===\n===\ntext\n";
        let sections = section_extents(body);
        assert_eq!((sections[0].body_start, sections[0].end_line_number), (2, 1));
        assert_eq!(section_body(body, &sections[0]), "");
        assert_eq!(edit_section_in_body(body, &sections[0], Append, "x"), "A\n\nx\n\n===\n===\ntext\n");
    }

    #[test]
    fn headings_in_fences_and_frontmatter_are_ignored() {
        let body = "---\n# yaml: comment\n---\n# Real\n~~~\n# code\n~~~\n";
        assert_eq!(note_sections(body), [sec(1, "Real", 3, 8)]);
        assert_eq!(section_body(body, &find_section(body, "real", None).unwrap()), "~~~\n# code\n~~~");
        assert!(find_section(body, "code", None).is_err());
    }

    #[test]
    fn closing_hashes_are_not_part_of_the_heading() {
        let body = "## Notes ##\n\nhi\n";
        assert_eq!(note_sections(body), [sec(2, "Notes", 0, 4)]);
        assert_eq!(edit(body, "### notes", Append, "there"), "## Notes ##\n\nhi\n\nthere\n");
        // `heading` is trimmed, then its leading #s, then trimmed again.
        assert!(find_section(body, "  ## NOTES  ", None).is_ok());
    }

    #[test]
    fn more_than_twenty_headings_are_cut_short() {
        let body: String = (1..=22).map(|i| format!("# H{i}\n")).collect();
        let listed: Vec<String> = (1..=20).map(|i| format!("H{i}")).collect();
        assert_eq!(
            find_section(&body, "Zed", None),
            Err(format!("No heading \"Zed\". Headings: {} | ....", listed.join(" | ")))
        );
        let body: String = (1..=20).map(|i| format!("# H{i}\n")).collect();
        assert_eq!(
            find_section(&body, "Zed", None),
            Err(format!("No heading \"Zed\". Headings: {}.", listed.join(" | ")))
        );
    }

    #[test]
    fn errors_quote_the_heading_the_go_way() {
        assert_eq!(
            find_section("# A\n", "say \"hi\"\t\\ \u{7}\u{1}\u{7f}é", None),
            Err("No heading \"say \\\"hi\\\"\\t\\\\ \\a\\x01\\x7fé\". Headings: A.".into())
        );
        assert_eq!(go_quote("\u{8}\u{c}\n\r\u{b}\u{1f}"), "\"\\b\\f\\n\\r\\v\\x1f\"");
        assert_eq!(go_quote(""), "\"\"");
    }

    #[test]
    fn append_to_a_section_with_subheadings_lands_above_the_first() {
        let body = "# Top\n\nintro\n\n## Sub\n\ndeep\n";
        assert_eq!(edit(body, "Top", Append, "more"), "# Top\n\nintro\n\nmore\n\n## Sub\n\ndeep\n");
        // A heading straight over its subheading gets a blank line, its text,
        // and a blank line before the subheading.
        assert_eq!(edit("# Top\n## Sub\nx\n", "Top", Append, "a"), "# Top\n\na\n\n## Sub\nx\n");
        // Replace takes the subheadings with it.
        assert_eq!(edit(body, "Top", Replace, "all new"), "# Top\n\nall new\n");
    }

    #[test]
    fn append_joins_lists_of_any_marker() {
        assert_eq!(edit("# L\n\n1. one\n", "L", Append, "2) two"), "# L\n\n1. one\n2) two\n");
        assert_eq!(edit("# L\n\n* one\n", "L", Append, "+ two\n- three"), "# L\n\n* one\n+ two\n- three\n");
        // A paragraph after a list, or a list after a paragraph, stands apart.
        assert_eq!(edit("# L\n\n- one\n", "L", Append, "text"), "# L\n\n- one\n\ntext\n");
        assert_eq!(edit("# L\n\ntext\n", "L", Append, "- one"), "# L\n\ntext\n\n- one\n");
        // `-x` is no list item.
        assert_eq!(edit("# L\n\n- one\n", "L", Append, "-x"), "# L\n\n- one\n\n-x\n");
    }

    #[test]
    fn prepend_and_replace_on_empty_and_last_sections() {
        // An empty section gets one blank line under its heading.
        assert_eq!(edit("# A\n# B\n", "A", Prepend, "p"), "# A\n\np\n\n# B\n");
        assert_eq!(edit("# A\n# B", "B", Append, "x"), "# A\n# B\n\nx\n");
        // A blank line kept as written by prepend and append.
        assert_eq!(edit("# A\n\n\nold\n", "A", Prepend, "new"), "# A\n\n\nnew\n\nold\n");
        assert_eq!(edit("# A\nold\n", "A", Append, "new"), "# A\nold\n\nnew\n");
        // Replace with nothing leaves the heading alone, the note still ending
        // in one newline; in the middle, one blank line before the next.
        assert_eq!(edit("# A\n\nold\n\n\n", "A", Replace, ""), "# A\n");
        assert_eq!(edit("# A\n\nold\n# B\n", "A", Replace, "\n\n"), "# A\n\n# B\n");
        // The last section keeps the note's final newline, and gains one.
        assert_eq!(edit("# A\n\nold", "A", Replace, "new"), "# A\n\nnew\n");
        assert_eq!(edit("# A\n\nold\n", "A", Append, "new\n\n\n"), "# A\n\nold\n\nnew\n");
    }

    #[test]
    fn duplicate_headings_list_every_line() {
        let body = "# X\n## X\n### x\n";
        assert_eq!(
            find_section(body, "x", None),
            Err("Heading \"x\" appears more than once, on lines 0, 1, 2; pass lineNumber to pick one.".into())
        );
        let s = find_section(body, "x", Some(1)).unwrap();
        assert_eq!((s.level, s.line_number, s.end_line_number, s.own_end), (2, 1, 4, 2));
    }

    #[test]
    fn split_and_slice_match_js() {
        assert_eq!(split_note_lines(""), [""]);
        assert_eq!(split_note_lines("a\r\nb\rc\n"), ["a", "b\rc", ""]);
        assert_eq!(split_note_lines("a\r\r\nb"), ["a\r", "b"]);
        assert_eq!(split_note_lines("a\r"), ["a\r"]);
        let lines = ["a", "b"];
        assert!(js_slice(&lines, 3, 5).is_empty());
        assert!(js_slice(&lines, 2, 1).is_empty());
        assert_eq!(js_slice(&lines, 1, 9), ["b"]);
    }
}
