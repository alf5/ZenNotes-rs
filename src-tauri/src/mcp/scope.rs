//! Scoped access for the MCP server (ZenNotes/tui#21): what an MCP client may
//! see and change, fixed when the server starts from its own launch flags
//! (`--allow-path`, `--deny-path`, `--read-only`, `--access`). No tool
//! argument can name or widen a scope; tools only narrow inside it.
//!
//! Port of packages/shared-domain/src/mcp-scope.ts, the normative copy of the
//! rules, which upstream's bundled server and the Go `zn mcp` both follow. The
//! shared fixture packages/bridge-contract/fixtures/mcp-scope.json holds all
//! of them (this one included, see scoped.rs's tests) to the same bytes.

use std::cmp::Ordering;
use std::sync::LazyLock;

use fancy_regex::Regex;
use serde_json::{json, Value};

/// How much a client may do to what it can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    Read,
    Comment,
    Write,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Comment => "comment",
            Access::Write => "write",
        }
    }
}

/// Whether `have` allows what `need` asks for.
pub fn access_at_least(have: Access, need: Access) -> bool {
    have >= need
}

/// An access level plus the paths a client may see. An empty `allow` means
/// the whole vault; a non-empty one shows only what it names (default-deny),
/// and `deny` hides paths even inside it. Entries are vault-relative paths
/// exactly as the tools report them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub access: Access,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// The `zennotes-rs mcp` flags a scope is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeFlags {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub access: Option<String>,
    pub read_only: bool,
}

/// Reads `read`, `comment` or `write`, and the spellings people reach for.
pub fn parse_access(raw: &str) -> Result<Access, String> {
    match js_trim(raw).to_lowercase().as_str() {
        "read" | "read-only" | "readonly" => Ok(Access::Read),
        "comment" => Ok(Access::Comment),
        "write" | "edit" => Ok(Access::Write),
        _ => Err(format!("--access must be read, comment or write, not {}", go_quote(raw))),
    }
}

/// Builds a scope from the flags. `read_only` lowers access to read whatever
/// `access` says; no access at all is write, the behavior without a scope. A
/// bad entry is an error: it never silently widens what a client sees.
pub fn new_scope(flags: &ScopeFlags) -> Result<Scope, String> {
    let mut access = Access::Write;
    if let Some(raw) = flags.access.as_deref().filter(|a| !js_trim(a).is_empty()) {
        access = parse_access(raw)?;
    }
    if flags.read_only {
        access = Access::Read;
    }
    Ok(Scope {
        access,
        allow: normalize_entries("--allow-path", &flags.allow)?,
        deny: normalize_entries("--deny-path", &flags.deny)?,
    })
}

/// The scope the flags ask for, or None when none were given: full access,
/// exactly as before scopes existed.
pub fn scope_from_flags(flags: &ScopeFlags) -> Result<Option<Scope>, String> {
    if flags.allow.is_empty()
        && flags.deny.is_empty()
        && flags.access.as_deref().is_none_or(str::is_empty)
        && !flags.read_only
    {
        return Ok(None);
    }
    new_scope(flags).map(Some)
}

/// Validates entries the way the Tasks folder lists are validated (forward
/// slashes, trimmed segments, no `.` or `..`, at most 512 characters,
/// duplicates dropped, order kept), and refuses hidden entries, which never
/// hold anything a client can see. A leading slash is accepted: an entry is
/// always vault-relative.
pub fn normalize_entries(flag: &str, values: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in values {
        let mut parts: Vec<&str> = Vec::new();
        let replaced = raw.replace('\\', "/");
        for seg in replaced.split('/') {
            let s = js_trim(seg);
            if s.is_empty() {
                continue;
            }
            if s == "." || s == ".." {
                return Err(format!(
                    "{flag} {}: . and .. are not allowed; name a folder or note inside the vault",
                    go_quote(raw)
                ));
            }
            if s.starts_with('.') {
                return Err(format!("{flag} {} names a hidden folder, which an assistant never sees", go_quote(raw)));
            }
            if s.contains('\0') {
                return Err(format!("{flag} {} is not a valid path", go_quote(raw)));
            }
            parts.push(s);
        }
        if parts.is_empty() {
            return Err(format!("{flag} {} is empty; name a folder or note inside the vault", go_quote(raw)));
        }
        let joined = parts.join("/");
        if joined.encode_utf16().count() > 512 {
            return Err(format!("{flag} {} is longer than 512 characters", go_quote(raw)));
        }
        if !out.contains(&joined) {
            out.push(joined);
        }
    }
    Ok(out)
}

/// Whether the scope narrows the paths at all.
pub fn is_restricted(scope: &Scope) -> bool {
    !scope.allow.is_empty() || !scope.deny.is_empty()
}

/// Whether the scope changes anything compared with full access.
pub fn is_active(scope: Option<&Scope>) -> bool {
    scope.is_some_and(|s| is_restricted(s) || s.access != Access::Write)
}

/* ---------- Matching ---------------------------------------------------- */

static NONSPACING_MARK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Mn}").expect("static regex"));

fn nfd(value: &str) -> String {
    icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(value).into_owned()
}

pub fn nfc(value: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize(value).into_owned()
}

/// Lower-cases one code point at a time, as upstream does to keep
/// JavaScript's context-sensitive final sigma out of it.
fn lower_each(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

/// Case and accent folding: decompose, drop the nonspacing marks, recompose,
/// lower-case one code point at a time. The Go side is `search.Fold`.
pub fn fold(value: &str) -> String {
    let decomposed = nfd(value);
    let stripped = NONSPACING_MARK_RE.replace_all(&decomposed, "");
    lower_each(&nfc(&stripped))
}

/// Case and Unicode form only, which a volume can ignore. Accents stay:
/// `Café` and `Cafe` are two folders, side by side on any volume.
pub fn fold_case(value: &str) -> String {
    lower_each(&nfc(value))
}

/// The segment-prefix rule of the Tasks folder lists: `Projects` covers
/// `Projects/x.md`, never `Projects2/x.md`.
pub fn is_inside(path: &str, entry: &str) -> bool {
    path == entry || (path.len() > entry.len() && path.starts_with(entry) && path.as_bytes()[entry.len()] == b'/')
}

/// The same rule after case and accent folding. Deny entries match this way
/// on purpose: on a case- or normalization-insensitive volume a near miss
/// must over-hide, never leak.
pub fn is_inside_folded(path: &str, entry: &str) -> bool {
    is_inside(&fold(path), &fold(entry))
}

/// Sorts by code point, which is UTF-8 byte order, as Go's sort.Strings does.
pub fn compare_paths(a: &str, b: &str) -> Ordering {
    a.as_bytes().cmp(b.as_bytes())
}

/* ---------- Tools ------------------------------------------------------- */

/// What a tool needs. `UnscopedWrite` acts on content regardless of path
/// (empty_trash deletes every trashed note), so no path-restricted client
/// gets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    Read,
    Comment,
    Write,
    UnscopedWrite,
}

/// Every MCP tool and what it needs. A tool missing here is treated as
/// `UnscopedWrite`, and a test fails until it is classified.
pub const TOOL_ACCESS: &[(&str, ToolClass)] = &[
    ("vault_info", ToolClass::Read),
    ("list_notes", ToolClass::Read),
    ("list_folders", ToolClass::Read),
    ("list_assets", ToolClass::Read),
    ("read_asset", ToolClass::Read),
    ("read_note", ToolClass::Read),
    ("note_sections", ToolClass::Read),
    ("daily_note", ToolClass::Read),
    ("search_text", ToolClass::Read),
    ("search_by_title", ToolClass::Read),
    ("search_by_tag", ToolClass::Read),
    ("list_tags", ToolClass::Read),
    ("backlinks", ToolClass::Read),
    ("list_tasks", ToolClass::Read),
    ("list_comments", ToolClass::Read),
    ("add_comment", ToolClass::Comment),
    ("reply_to_comment", ToolClass::Comment),
    ("resolve_comment", ToolClass::Comment),
    ("toggle_task", ToolClass::Write),
    ("update_task", ToolClass::Write),
    ("edit_section", ToolClass::Write),
    ("upload_asset", ToolClass::Write),
    ("rename_asset", ToolClass::Write),
    ("move_asset", ToolClass::Write),
    ("duplicate_asset", ToolClass::Write),
    ("delete_asset", ToolClass::Write),
    ("restore_asset", ToolClass::Write),
    ("write_note", ToolClass::Write),
    ("append_to_note", ToolClass::Write),
    ("prepend_to_note", ToolClass::Write),
    ("insert_at_line", ToolClass::Write),
    ("replace_in_note", ToolClass::Write),
    ("create_note", ToolClass::Write),
    ("rename_note", ToolClass::Write),
    ("move_note", ToolClass::Write),
    ("duplicate_note", ToolClass::Write),
    ("archive_note", ToolClass::Write),
    ("unarchive_note", ToolClass::Write),
    ("move_to_trash", ToolClass::Write),
    ("restore_from_trash", ToolClass::Write),
    ("delete_note", ToolClass::Write),
    ("create_folder", ToolClass::Write),
    ("rename_folder", ToolClass::Write),
    ("delete_folder", ToolClass::Write),
    ("empty_trash", ToolClass::UnscopedWrite),
];

/// Tools that may destroy content rather than add to it or move it.
#[cfg(test)]
pub const DESTRUCTIVE_TOOLS: &[&str] =
    &["write_note", "replace_in_note", "edit_section", "delete_note", "delete_folder", "empty_trash"];

pub fn tool_class(name: &str) -> ToolClass {
    TOOL_ACCESS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c).unwrap_or(ToolClass::UnscopedWrite)
}

/// Whether a scope offers a tool. No scope is full access. A tool a scope
/// does not offer is not listed at all, so the model never plans around it.
pub fn is_tool_offered(name: &str, scope: Option<&Scope>) -> bool {
    let Some(scope) = scope else { return true };
    match tool_class(name) {
        ToolClass::Read => true,
        ToolClass::Comment => access_at_least(scope.access, Access::Comment),
        ToolClass::Write => scope.access == Access::Write,
        ToolClass::UnscopedWrite => scope.access == Access::Write && !is_restricted(scope),
    }
}

/// What tools/list says about a tool (served from tools.json, which upstream
/// generated from this same rule; kept for the test that holds them equal).
#[cfg(test)]
pub fn tool_annotations(name: &str) -> Value {
    if tool_class(name) == ToolClass::Read {
        return json!({ "readOnlyHint": true, "idempotentHint": false, "openWorldHint": false });
    }
    json!({
        "readOnlyHint": false,
        "destructiveHint": DESTRUCTIVE_TOOLS.contains(&name),
        "idempotentHint": false,
        "openWorldHint": false
    })
}

/// The answer to a tool above the scope's access level. It names no path, so
/// it says nothing about the vault.
pub fn access_refusal(scope: &Scope) -> String {
    match scope.access {
        Access::Read => "This assistant can only read this vault.".into(),
        Access::Comment => "This assistant can only read this vault and comment on notes.".into(),
        Access::Write => "This assistant cannot use that tool on a vault it only partly sees.".into(),
    }
}

/// Appended to the server instructions while a scope is active, so the model
/// neither guesses at hidden notes nor tries tools it does not have.
pub fn scope_instructions(scope: &Scope) -> String {
    let mut text = String::from("\n\n## Access\n\n");
    if is_restricted(scope) {
        text.push_str("This server shows part of the user's vault. Paths and notes you are not shown do not exist for you: do not guess at them, and treat a [[link]] that does not resolve as pointing outside what you can see. ");
    }
    match scope.access {
        Access::Read => text.push_str("You can read but not change anything. "),
        Access::Comment => text.push_str("You can read and comment on notes, but not change them. "),
        Access::Write => {}
    }
    format!("{text}Call `vault_info` to see what you can reach.")
}

/// The sentence vault_info adds to its notes when the paths are narrowed.
pub const VAULT_INFO_NOTE: &str = " You see part of this vault: `access.allow` lists what was shared (empty means everything except some hidden paths). Anything else does not exist for you.";

/// The scope as vault_info reports it: allow entries in the vault's spelling
/// and the ones that match nothing. Deny entries are never named, since the
/// names alone can say too much. `blocked` is why the scope shares nothing,
/// when it does not.
pub fn access_info(scope: &Scope, allow: &[String], missing_allow: &[String], blocked: Option<&str>) -> Value {
    let sorted = |list: &[String]| {
        let mut v = list.to_vec();
        v.sort_by(|a, b| compare_paths(a, b));
        v
    };
    let mut info = json!({
        "level": scope.access.as_str(),
        "partial": is_restricted(scope),
        "allow": sorted(allow),
        "missingAllow": sorted(missing_allow),
    });
    if let Some(reason) = blocked.filter(|b| !b.is_empty()) {
        info["blocked"] = json!(reason);
    }
    info
}

/* ---------- Messages ---------------------------------------------------- */

/// Go's `%q` for the strings a flag can hold, so a refusal reads byte for
/// byte the same from either engine: printable text as is, ASCII controls
/// escaped the way strconv.Quote escapes them.
pub fn go_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// ECMAScript WhiteSpace + LineTerminator, what `String.prototype.trim` strips.
pub fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
            | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(allow: &[&str], deny: &[&str], access: Option<&str>, read_only: bool) -> ScopeFlags {
        ScopeFlags {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            access: access.map(str::to_string),
            read_only,
        }
    }

    #[test]
    fn shared_fixture_flag_cases() {
        let raw = include_str!("../../../packages/bridge-contract/fixtures/mcp-scope.json");
        let fixture: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(fixture["schemaVersion"], 1);
        for case in fixture["flagCases"].as_array().unwrap() {
            let f = &case["flags"];
            let list = |key: &str| -> Vec<String> {
                f[key].as_array().map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect()).unwrap_or_default()
            };
            let parsed = new_scope(&ScopeFlags {
                allow: list("allow"),
                deny: list("deny"),
                access: f["access"].as_str().map(str::to_string),
                read_only: f["readOnly"].as_bool().unwrap_or(false),
            });
            let id = case["id"].as_str().unwrap();
            if let Some(error) = case["error"].as_str() {
                assert_eq!(parsed, Err(error.to_string()), "{id}");
            } else {
                let scope = parsed.unwrap_or_else(|e| panic!("{id}: {e}"));
                let want = &case["scope"];
                assert_eq!(scope.access.as_str(), want["access"], "{id}");
                assert_eq!(json!(scope.allow), want["allow"], "{id}");
                assert_eq!(json!(scope.deny), want["deny"], "{id}");
            }
        }
    }

    #[test]
    fn no_flags_is_no_scope() {
        assert_eq!(scope_from_flags(&ScopeFlags::default()), Ok(None));
        let read = scope_from_flags(&flags(&[], &[], None, true)).unwrap().unwrap();
        assert_eq!(read.access, Access::Read);
        assert!(is_active(Some(&read)));
        let write = new_scope(&flags(&[], &[], Some("write"), false)).unwrap();
        assert!(!is_active(Some(&write)));
    }

    #[test]
    fn folding_and_matching() {
        assert_eq!(fold("Café"), "cafe");
        assert_eq!(fold("CAFE\u{301}"), "cafe");
        assert_eq!(fold("ΟΔΟΣ"), "οδοσ");
        assert_eq!(fold_case("Café"), "café");
        assert!(is_inside("Projects/x.md", "Projects"));
        assert!(!is_inside("Projects2/x.md", "Projects"));
        assert!(is_inside_folded("projects/Café/x.md", "Projects/cafe"));
        assert_eq!(compare_paths("B", "a"), Ordering::Less);
    }

    #[test]
    fn tools_offered_by_access() {
        let read = new_scope(&flags(&[], &[], Some("read"), false)).unwrap();
        let comment = new_scope(&flags(&["x"], &[], Some("comment"), false)).unwrap();
        let partial = new_scope(&flags(&["x"], &[], None, false)).unwrap();
        assert!(is_tool_offered("list_notes", Some(&read)));
        assert!(!is_tool_offered("add_comment", Some(&read)));
        assert!(is_tool_offered("add_comment", Some(&comment)));
        assert!(!is_tool_offered("write_note", Some(&comment)));
        assert!(is_tool_offered("write_note", Some(&partial)));
        assert!(!is_tool_offered("empty_trash", Some(&partial)));
        assert!(is_tool_offered("empty_trash", None));
        assert!(!is_tool_offered("not_a_tool", Some(&partial)));
    }

    #[test]
    fn go_quote_escapes_like_go() {
        assert_eq!(go_quote("a\"b\\c\n\u{0}"), "\"a\\\"b\\\\c\\n\\x00\"");
        assert_eq!(go_quote("Café"), "\"Café\"");
    }
}
