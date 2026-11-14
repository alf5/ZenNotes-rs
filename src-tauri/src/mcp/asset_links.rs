//! Rewriting the links in a note when an asset is renamed or moved, for the
//! MCP `rename_asset` / `move_asset` tools. Port of upstream
//! packages/shared-domain/src/asset-link-rename.ts (`rewriteAssetReferences`,
//! its options and result) together with what it reaches:
//! - packages/shared-domain/src/wikilink-rename.ts: `RenameNoteRef`,
//!   `isPathLikeWikilinkTarget`, `resolveWikilinkTarget` and the private
//!   helpers under it, `wikilinkAliasStart`. (`rewriteWikilinksForRename`,
//!   the note-rename rewrite, is not reached from here and is not ported.)
//! - packages/shared-domain/src/asset-path-resolution.ts in full:
//!   `AssetPathRef`, `stripQueryAndHash`, `decodeHrefPath`, `posixJoin`,
//!   `posixNormalize`, `resolveAssetReference`, `resolveAssetPathAmong`.
//!
//! Upstream's MCP (apps/desktop/src/mcp/vault-ops.ts, `rewriteAssetLinks`)
//! calls it as `rewriteAssetReferences(content.body, assetsBefore, note.path,
//! oldRel, newRel, { notes: nameable, fullPath: rewriteIn != null })`; the
//! two options are the last two arguments of [`rewrite_asset_references`].
//!
//! JS semantics kept, the same rules as the `//!` header of tasks.rs:
//! - JS `\s` and `String.prototype.trim` are ECMAScript WhiteSpace plus
//!   LineTerminator (`ws!` / `is_js_ws` / `js_trim`), not Rust's Unicode
//!   White_Space.
//! - JS `\d` is ASCII; the scheme test spells it out.
//! - `/\.md$/i` without the `u` flag folds ASCII only (a non-ASCII character
//!   never canonicalizes to an ASCII one), so it is an ASCII case-insensitive
//!   suffix test.
//! - Indexes are byte offsets. Every one comes from searching for an ASCII
//!   character (`|`, `/`, `#`, `?`, `<`, `>`), so it lands on the same
//!   character boundary JS's UTF-16 index does, and the slices agree.
//! - `decodeURIComponent` / `encodeURIComponent` are re-implemented to the
//!   ECMAScript algorithm (a malformed escape or an escape that is not one
//!   UTF-8 code point is the URIError upstream catches), not borrowed from a
//!   URL crate whose rules differ.
//! - `toLowerCase` is `str::to_lowercase`; both apply the full Unicode
//!   mapping with the Final_Sigma rule. The only drift left is a difference in
//!   Unicode version between Rust's tables and the JS engine's.

use std::sync::LazyLock;

use fancy_regex::Regex;

/* ---------- JS regex semantics ---------------------------------------- */

/// ECMAScript `\s`, as a character-class body.
macro_rules! ws {
    () => {
        r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}"
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

/// `s.slice(1, -1)` for a string already known to start with `<` and end with
/// `>` (two ASCII bytes, so the cut is on a character boundary).
fn strip_angles(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('<') && s.ends_with('>') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// `/\.md$/i.test(value)`.
fn ends_with_md(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() >= 3 && b[b.len() - 3..].eq_ignore_ascii_case(b".md")
}

/// ECMAScript `decodeURIComponent`, or `None` where it throws a URIError: a
/// `%` without two hex digits after it, a stray continuation byte, a lead
/// byte the continuation escapes do not complete, or bytes that are not one
/// valid UTF-8 code point (overlong forms, surrogates, past U+10FFFF).
fn decode_uri_component(s: &str) -> Option<String> {
    fn hex_byte(bytes: &[u8], at: usize) -> Option<u8> {
        let hi = (*bytes.get(at + 1)? as char).to_digit(16)?;
        let lo = (*bytes.get(at + 2)? as char).to_digit(16)?;
        Some((hi * 16 + lo) as u8)
    }
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let lead = hex_byte(bytes, i)?;
        i += 3;
        if lead < 0x80 {
            out.push(lead);
            continue;
        }
        let n = lead.leading_ones() as usize;
        if n == 1 || n > 4 {
            return None;
        }
        let mut seq = [lead, 0, 0, 0];
        for slot in seq.iter_mut().take(n).skip(1) {
            if bytes.get(i) != Some(&b'%') {
                return None;
            }
            let b = hex_byte(bytes, i)?;
            if b & 0xC0 != 0x80 {
                return None;
            }
            *slot = b;
            i += 3;
        }
        std::str::from_utf8(&seq[..n]).ok()?;
        out.extend_from_slice(&seq[..n]);
    }
    String::from_utf8(out).ok()
}

/// ECMAScript `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * '
/// ( )` as `%XX` of its UTF-8 bytes, upper-case hex. (JS throws on a lone
/// surrogate, which a Rust string cannot hold.)
fn encode_uri_component(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
        {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

/// `path.slice(0, path.lastIndexOf('/'))` when the path has a `/`, else ''.
fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// `path.split('/').filter(Boolean).pop()`.
fn last_segment(path: &str) -> Option<&str> {
    path.split('/').rfind(|s| !s.is_empty())
}

/* ---------- asset-path-resolution.ts --------------------------------- */

/// An asset as the resolver sees it (upstream `AssetPathRef`): its
/// vault-relative POSIX path. Upstream's listing entries carry more (a name,
/// a size), but nothing here reads anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetRef<'a> {
    pub path: &'a str,
}

/// `stripQueryAndHash`: the href before its first `#`, then before its first
/// `?`.
pub fn strip_query_and_hash(href: &str) -> &str {
    let before_hash = href.split('#').next().unwrap_or(href);
    before_hash.split('?').next().unwrap_or(before_hash)
}

/// `decodeHrefPath`: the path part of an href, percent-decoded when it
/// decodes, else as written.
pub fn decode_href_path(value: &str) -> String {
    let cleaned = strip_query_and_hash(value);
    decode_uri_component(cleaned).unwrap_or_else(|| cleaned.to_string())
}

/// `posixJoin`.
pub fn posix_join(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.to_string();
    }
    if b.is_empty() {
        return a.to_string();
    }
    if a.ends_with('/') {
        return format!("{a}{b}");
    }
    format!("{a}/{b}")
}

/// `posixNormalize`: drops empty and `.` segments and folds `..`; a path that
/// climbs above its start comes back as exactly `..`.
pub fn posix_normalize(input: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in input.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if out.is_empty() {
                return "..".to_string();
            }
            out.pop();
        } else {
            out.push(part);
        }
    }
    out.join("/")
}

/// Which of the three readings resolved a reference (upstream
/// `AssetReferenceReading`). Lets a rewrite keep the author's style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetReferenceReading {
    /// `'note-relative'`
    NoteRelative,
    /// `'vault-root'`
    VaultRoot,
    /// `'basename'`
    Basename,
}

/// Upstream `AssetReferenceResolution`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetReferenceResolution {
    /// Vault-relative path of the asset the reference points at.
    pub path: String,
    pub reading: AssetReferenceReading,
    /// The reference was written with a leading `/` (vault-root, spelled out).
    pub absolute: bool,
}

/// `/^[a-zA-Z][a-zA-Z\d+.-]*:/`: the href names a URL scheme.
fn has_url_scheme(s: &str) -> bool {
    let b = s.as_bytes();
    if !b.first().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest = b[1..]
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-')));
    rest.is_some_and(|at| b[1 + at] == b':')
}

/// `resolveAssetReference`: the asset an href or wikilink target names, tried
/// relative to the note's folder, then to the vault root, then as a basename
/// unique in the vault (case-insensitive).
pub fn resolve_asset_reference(
    assets: &[AssetRef<'_>],
    note_path: &str,
    href: &str,
) -> Option<AssetReferenceResolution> {
    let trimmed = js_trim(href);
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
        return None;
    }
    if has_url_scheme(trimmed) {
        return None;
    }

    let note_dir = dir_of(note_path);
    let decoded_href = decode_href_path(trimmed);
    let is_absolute = decoded_href.starts_with('/');
    let target = if is_absolute {
        decoded_href.trim_start_matches('/').to_string()
    } else if !note_dir.is_empty() {
        posix_join(note_dir, &decoded_href)
    } else {
        decoded_href.clone()
    };
    let target = posix_normalize(&target);
    if target.starts_with("../") || target == ".." {
        return None;
    }

    if assets.iter().any(|asset| asset.path == target) {
        let reading = if is_absolute || note_dir.is_empty() {
            AssetReferenceReading::VaultRoot
        } else {
            AssetReferenceReading::NoteRelative
        };
        return Some(AssetReferenceResolution { path: target, reading, absolute: is_absolute });
    }

    if !is_absolute && !note_dir.is_empty() {
        let root_target = posix_normalize(&decoded_href);
        if !root_target.is_empty()
            && root_target != target
            && !root_target.starts_with("../")
            && root_target != ".."
            && assets.iter().any(|asset| asset.path == root_target)
        {
            return Some(AssetReferenceResolution {
                path: root_target,
                reading: AssetReferenceReading::VaultRoot,
                absolute: false,
            });
        }
    }

    let target_base = last_segment(&target)?.to_lowercase();
    let mut matches = assets
        .iter()
        .filter(|asset| last_segment(asset.path).is_some_and(|b| b.to_lowercase() == target_base));
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some(AssetReferenceResolution {
            path: only.path.to_string(),
            reading: AssetReferenceReading::Basename,
            absolute: false,
        }),
        _ => None,
    }
}

/// `resolveAssetPathAmong`: just the path of [`resolve_asset_reference`].
/// Nothing in the rewrite calls it; it is the one-link lookup upstream's
/// follow-link and Connections paths use, kept beside the resolver.
#[allow(dead_code)]
pub fn resolve_asset_path_among(
    assets: &[AssetRef<'_>],
    note_path: &str,
    href: &str,
) -> Option<String> {
    resolve_asset_reference(assets, note_path, href).map(|r| r.path)
}

/* ---------- wikilink-rename.ts --------------------------------------- */

/// A note a wikilink can name (upstream `RenameNoteRef`). `folder` is the
/// note's top-level area (`inbox`, `quick`, `archive`, `trash`): notes in the
/// trash never resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteRef<'a> {
    pub path: &'a str,
    pub title: &'a str,
    pub folder: &'a str,
}

const TOP_FOLDERS: [&str; 4] = ["inbox", "quick", "archive", "trash"];

/// `value.replace(/\\/g, '/').replace(/\/+/g, '/')`.
fn normalize_slashes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        let c = if c == '\\' { '/' } else { c };
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    out
}

/// `value.replace(/\.md$/i, '')`.
fn strip_md_extension(value: &str) -> &str {
    if ends_with_md(value) {
        &value[..value.len() - 3]
    } else {
        value
    }
}

/// `value.trim().toLowerCase()`.
fn normalize_for_compare(value: &str) -> String {
    js_trim(value).to_lowercase()
}

/// `.replace(/^\/+/, '').replace(/\/+$/, '')`.
fn trim_slashes(value: &str) -> &str {
    value.trim_start_matches('/').trim_end_matches('/')
}

/// `isPathLikeWikilinkTarget`: a target with a `/` or a `.md` resolves by
/// path; anything else by title.
pub fn is_path_like_wikilink_target(target: &str) -> bool {
    let trimmed = js_trim(target);
    trimmed.starts_with('/') || trimmed.contains('/') || ends_with_md(trimmed)
}

fn resolve_explicit_path<'s, 'a>(notes: &[&'s NoteRef<'a>], target: &str) -> Option<&'s NoteRef<'a>> {
    let normalized = normalize_slashes(js_trim(target));
    if normalized.is_empty() {
        return None;
    }
    let trimmed = trim_slashes(strip_md_extension(&normalized));
    if trimmed.is_empty() {
        return None;
    }

    let rel_path = if normalized.starts_with('/') {
        format!("inbox/{trimmed}.md")
    } else {
        let lower = trimmed.to_lowercase();
        if TOP_FOLDERS.iter().any(|folder| lower.starts_with(&format!("{folder}/"))) {
            format!("{trimmed}.md")
        } else {
            return None;
        }
    };

    let needle = normalize_for_compare(&rel_path);
    notes.iter().copied().find(|note| normalize_for_compare(note.path) == needle)
}

fn resolve_path_suffix<'s, 'a>(notes: &[&'s NoteRef<'a>], target: &str) -> Option<&'s NoteRef<'a>> {
    let normalized = normalize_slashes(js_trim(target));
    let trimmed = trim_slashes(strip_md_extension(&normalized));
    if trimmed.is_empty() {
        return None;
    }

    let suffix = normalize_for_compare(&format!("/{trimmed}.md"));
    let exact = normalize_for_compare(&format!("{trimmed}.md"));
    let mut matches = notes.iter().copied().filter(|note| {
        let path = normalize_for_compare(note.path);
        path == exact || path.ends_with(&suffix)
    });
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// `resolveWikilinkTarget`: the note a wikilink target names, by explicit or
/// unique suffix path when it reads as a path, else by title (first match,
/// case-insensitive). Notes in the trash are never named.
pub fn resolve_wikilink_target<'s, 'a>(notes: &'s [NoteRef<'a>], target: &str) -> Option<&'s NoteRef<'a>> {
    let visible: Vec<&'s NoteRef<'a>> = notes.iter().filter(|note| note.folder != "trash").collect();
    if is_path_like_wikilink_target(target) {
        return resolve_explicit_path(&visible, target).or_else(|| resolve_path_suffix(&visible, target));
    }
    let needle = normalize_for_compare(strip_md_extension(target));
    visible.into_iter().find(|note| normalize_for_compare(note.title) == needle)
}

/// `wikilinkAliasStart`: the byte offset where a wikilink's alias starts,
/// separator included, or `None` (upstream's -1). A table writes the
/// separator as `\|`, so a backslash right before the first pipe belongs to
/// the alias.
pub fn wikilink_alias_start(content: &str) -> Option<usize> {
    let pipe = content.find('|')?;
    if pipe > 0 && content.as_bytes()[pipe - 1] == b'\\' {
        Some(pipe - 1)
    } else {
        Some(pipe)
    }
}

/* ---------- asset-link-rename.ts ------------------------------------- */

/// Code first so references inside spans and fences are left untouched, then
/// a wikilink, then a markdown destination, matched from its `](` so the href
/// of an image nested inside a link is found as readily as the link's own.
/// Groups: 1 code, 2 embed `!`, 3 wikilink content, 4 href, 5 title.
static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(```[\s\S]*?```|~~~[\s\S]*?~~~|`[^`\n]*`)",
        r"|(!?)\[\[([^\]\n]+?)\]\]",
        r"|\]\([",
        ws!(),
        r"]*(<[^>\n]*>|[^)\n]+?)((?:[",
        ws!(),
        r#"]+(?:"[^"]*"|'[^']*'|\([^)]*\)))?)["#,
        ws!(),
        r"]*\)",
    ))
    .expect("static asset link regex")
});

/// Upstream `AssetLinkRewrite`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetLinkRewrite {
    pub body: String,
    /// Number of references rewritten.
    pub changed: usize,
    /// Set when a wikilink to the file had to stay as it was, since its target
    /// cannot hold the new path. (Upstream leaves the key out rather than
    /// setting it false.)
    pub stranded: bool,
}

/// `WIKILINK_SYNTAX = /[#?|[\]^]/`: what a wikilink target reads as
/// something other than a path.
fn has_wikilink_syntax(s: &str) -> bool {
    s.contains(['#', '?', '|', '[', ']', '^'])
}

/// `basenameOf`: everything after the last `/`.
fn basename_of(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

/// `splitSuffix`: a `#fragment` or `?query` belongs to the reference, not to
/// the file name.
fn split_suffix(reference: &str) -> (&str, &str) {
    match reference.find(['#', '?']) {
        Some(cut) => (&reference[..cut], &reference[cut..]),
        None => (reference, ""),
    }
}

/// `relativeTo`: POSIX path from directory `from_dir` ('' = vault root) to
/// `to_path`.
fn relative_to(from_dir: &str, to_path: &str) -> String {
    let from: Vec<&str> = from_dir.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = to_path.split('/').filter(|s| !s.is_empty()).collect();
    let mut shared = 0;
    while shared < from.len() && shared < to.len() && from[shared] == to[shared] {
        shared += 1;
    }
    let mut parts: Vec<&str> = vec![".."; from.len() - shared];
    parts.extend_from_slice(&to[shared..]);
    parts.join("/")
}

/// `encodeLike`: percent-encode `next` per segment when the author wrote
/// `original` encoded.
fn encode_like(original: &str, next: &str) -> String {
    match decode_uri_component(original) {
        Some(decoded) if decoded != original => next
            .split('/')
            .map(|segment| {
                if segment == ".." || segment == "." {
                    segment.to_string()
                } else {
                    encode_uri_component(segment)
                }
            })
            .collect::<Vec<_>>()
            .join("/"),
        _ => next.to_string(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefKind {
    Wikilink,
    Href,
}

/// `rewriteAssetReferences`: rewrite every reference in `body` that resolves
/// to the asset at `old_rel` so it points at `new_rel` (both vault-relative
/// POSIX paths). `assets_before` must be the pre-change listing; `note_path`
/// is the note holding `body`, since a markdown href resolves relative to it.
///
/// The last two arguments are upstream's options: `notes` is
/// `options.notes`, the notes a wikilink can name (a note wins over a file of
/// the same name, so such a link is left alone; an empty slice behaves as the
/// option left out), and `full_path` is `options.fullPath`, write the whole
/// path where a bare name would do. Upstream's MCP passes `notes: nameable`
/// and `fullPath: rewriteIn != null`.
pub fn rewrite_asset_references(
    body: &str,
    assets_before: &[AssetRef<'_>],
    note_path: &str,
    old_rel: &str,
    new_rel: &str,
    notes: &[NoteRef<'_>],
    full_path: bool,
) -> AssetLinkRewrite {
    let unchanged = || AssetLinkRewrite { body: body.to_string(), changed: 0, stranded: false };
    if new_rel.is_empty() || old_rel == new_rel {
        return unchanged();
    }
    if !body.contains("[[") && !body.contains("](") {
        return unchanged();
    }

    let note_dir = dir_of(note_path);
    // The listing as it reads after the change decides whether a bare file
    // name still names exactly one asset.
    let new_base = basename_of(new_rel);
    let new_base_lower = new_base.to_lowercase();
    let bare_unique = assets_before
        .iter()
        .map(|asset| if asset.path == old_rel { new_rel } else { asset.path })
        .filter(|path| basename_of(path).to_lowercase() == new_base_lower)
        .count()
        == 1;
    let bare_still_unique = bare_unique && !full_path;
    let mut stranded = false;

    let resolve_old = |reference: &str| -> Option<AssetReferenceResolution> {
        let inner = strip_angles(js_trim(reference));
        resolve_asset_reference(assets_before, note_path, inner).filter(|r| r.path == old_rel)
    };

    // A wikilink is written as a bare name or a vault-root path, never with
    // `..`; a markdown href is a real relative path and follows the file.
    let retarget = |reference: &str,
                    resolution: &AssetReferenceResolution,
                    kind: RefKind,
                    force_bare: bool|
     -> String {
        let angled = reference.starts_with('<') && reference.ends_with('>');
        let inner = if angled { strip_angles(reference) } else { reference };
        let (path, suffix) = split_suffix(inner);
        let bare = !path.contains('/');
        let next = if force_bare {
            new_base.to_string()
        } else if resolution.reading == AssetReferenceReading::Basename
            || (kind == RefKind::Wikilink && bare)
        {
            if bare_still_unique { new_base } else { new_rel }.to_string()
        } else if resolution.reading == AssetReferenceReading::NoteRelative && kind == RefKind::Href {
            relative_to(note_dir, new_rel)
        } else if resolution.absolute {
            format!("/{new_rel}")
        } else {
            new_rel.to_string()
        };
        let mut encoded = encode_like(path, &next);
        // In a folder's name these would read as the start of a fragment or a
        // query.
        if kind == RefKind::Href {
            encoded = encoded.replace('#', "%23").replace('?', "%3F");
        }
        let out = format!("{encoded}{suffix}");
        // A Markdown destination with a space or a parenthesis only stays a
        // link in angle brackets.
        if angled || (kind == RefKind::Href && out.contains([' ', '\t', '(', ')'])) {
            return format!("<{}>", out.replace('<', "%3C").replace('>', "%3E"));
        }
        out
    };

    let mut changed = 0usize;
    let mut out = String::with_capacity(body.len());
    let mut last = 0usize;
    // Captures can only fail on fancy_regex's backtrack limit, which a
    // pattern without fancy features never reaches; should it ever, the rest
    // of the body is kept as it is.
    for caps in TOKEN_RE.captures_iter(body) {
        let Ok(caps) = caps else { break };
        let Some(whole) = caps.get(0) else { continue };
        out.push_str(&body[last..whole.start()]);
        last = whole.end();
        let full = whole.as_str();

        if caps.get(1).is_some() {
            out.push_str(full);
            continue;
        }

        if let Some(content) = caps.get(3).map(|m| m.as_str()) {
            let embed = caps.get(2).map_or("", |m| m.as_str());
            let (target, rest) = match wikilink_alias_start(content) {
                Some(pipe) => (&content[..pipe], &content[pipe..]),
                None => (content, ""),
            };
            let Some(resolution) = resolve_old(target) else {
                out.push_str(full);
                continue;
            };
            // `[[Report#Heading]]` names the note as `[[Report]]` does.
            let doc = target.split(['#', '^']).next().unwrap_or(target);
            if resolve_wikilink_target(notes, doc).is_some() {
                out.push_str(full);
                continue;
            }
            // `![[Issue #5/pic.png]]` names a heading in a note, so a target
            // cannot hold such a path. A bare name is used where it will do.
            let mut force_bare = false;
            if has_wikilink_syntax(new_rel) {
                if has_wikilink_syntax(new_base) || (!full_path && !bare_unique) {
                    stranded = true;
                    out.push_str(full);
                    continue;
                }
                force_bare = true;
            }
            let next = format!(
                "{embed}[[{}{rest}]]",
                retarget(target, &resolution, RefKind::Wikilink, force_bare)
            );
            // A bare name that still resolves after a move reads exactly as
            // before.
            if next != full {
                changed += 1;
            }
            out.push_str(&next);
            continue;
        }

        let Some(href) = caps.get(4).map(|m| m.as_str()) else {
            out.push_str(full);
            continue;
        };
        let title = caps.get(5).map_or("", |m| m.as_str());
        let Some(resolution) = resolve_old(href) else {
            out.push_str(full);
            continue;
        };
        let next = format!("]({}{title})", retarget(href, &resolution, RefKind::Href, false));
        if next != full {
            changed += 1;
        }
        out.push_str(&next);
    }
    out.push_str(&body[last..]);

    AssetLinkRewrite { body: out, changed, stranded }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs<'a>(paths: &[&'a str]) -> Vec<AssetRef<'a>> {
        paths.iter().map(|path| AssetRef { path }).collect()
    }

    fn rw(body: &str, assets: &[&str], note: &str, old: &str, new: &str) -> AssetLinkRewrite {
        rewrite_asset_references(body, &refs(assets), note, old, new, &[], false)
    }

    fn done(body: &str, changed: usize) -> AssetLinkRewrite {
        AssetLinkRewrite { body: body.to_string(), changed, stranded: false }
    }

    /* asset-link-rename.test.ts ---------------------------------------- */

    const ASSETS: [&str; 6] = [
        "assets/old.png",
        "assets/other.png",
        "assets/old name.png",
        "docs/manual.pdf",
        "assets/dup.png",
        "docs/dup.png",
    ];
    const NOTE: &str = "inbox/Daily/2026-09-15.md";

    fn rename(body: &str) -> AssetLinkRewrite {
        rw(body, &ASSETS, NOTE, "assets/old.png", "assets/new.png")
    }

    fn rename_to(body: &str, old: &str, new: &str) -> AssetLinkRewrite {
        rw(body, &ASSETS, NOTE, old, new)
    }

    fn mv(body: &str) -> AssetLinkRewrite {
        rw(body, &ASSETS, NOTE, "assets/old.png", "media/shots/old.png")
    }

    #[test]
    fn rewrites_the_vault_relative_embed_the_paste_flow_writes() {
        assert_eq!(rename("Shot: ![[assets/old.png]]\n"), done("Shot: ![[assets/new.png]]\n", 1));
    }

    #[test]
    fn keeps_size_hints_aliases_fragments_and_plain_wikilinks() {
        let body = "![[assets/old.png|300]] [[assets/old.png|the shot]] [[/assets/old.png#top]]";
        assert_eq!(
            rename(body).body,
            "![[assets/new.png|300]] [[assets/new.png|the shot]] [[/assets/new.png#top]]"
        );
    }

    #[test]
    fn rewrites_markdown_images_and_links_keeping_titles_and_relative_paths() {
        let body = "![alt](assets/old.png \"Title\")\n[open](../../assets/old.png)\n[page two](/assets/old.png#page=2)";
        assert_eq!(
            rename(body).body,
            "![alt](assets/new.png \"Title\")\n[open](../../assets/new.png)\n[page two](/assets/new.png#page=2)"
        );
    }

    #[test]
    fn keeps_percent_encoding_and_angle_brackets_around_spaced_names() {
        let body = "![](assets/old%20name.png) ![](<assets/old name.png>) ![[assets/old name.png]]";
        let out = rename_to(body, "assets/old name.png", "assets/new name.png");
        assert_eq!(out.body, "![](assets/new%20name.png) ![](<assets/new name.png>) ![[assets/new name.png]]");
        assert_eq!(out.changed, 3);
    }

    #[test]
    fn rewrites_both_hrefs_of_an_image_nested_inside_a_link() {
        assert_eq!(
            rename("[![shot](assets/old.png)](assets/old.png)").body,
            "[![shot](assets/new.png)](assets/new.png)"
        );
    }

    #[test]
    fn follows_a_bare_basename_only_while_it_is_unique() {
        assert_eq!(rename("![[old.png]] ![](old.png)").body, "![[new.png]] ![](new.png)");
        // Two `dup.png` files: the link was already ambiguous.
        assert_eq!(rename_to("![[dup.png]]", "assets/dup.png", "assets/renamed.png"), done("![[dup.png]]", 0));
    }

    #[test]
    fn leaves_code_other_assets_notes_and_urls_untouched() {
        let body = [
            "`![[assets/old.png]]` and `[x](assets/old.png)`",
            "```",
            "![[assets/old.png]]",
            "```",
            "~~~md",
            "![](assets/old.png)",
            "~~~",
            "![[assets/other.png]] [[Old Note]] [[docs/manual.pdf]]",
            "[web](https://example.com/assets/old.png) ![](//cdn/assets/old.png)",
        ]
        .join("\n");
        assert_eq!(rename(&body), done(&body, 0));
    }

    #[test]
    fn does_nothing_when_the_name_did_not_change() {
        let body = "![[assets/old.png]]";
        assert_eq!(rename_to(body, "assets/old.png", "assets/old.png"), done(body, 0));
        assert_eq!(rename_to(body, "assets/old.png", ""), done(body, 0));
    }

    #[test]
    fn handles_a_case_only_rename() {
        assert_eq!(rename_to("![[assets/old.png]]", "assets/old.png", "assets/OLD.png").body, "![[assets/OLD.png]]");
    }

    #[test]
    fn handles_an_escaped_pipe() {
        let cases: [(&str, &str, &str, usize); 9] = [
            ("| ![[old.png\\|300]] |", "assets/new.png", "| ![[new.png\\|300]] |", 1),
            ("| ![[assets/old.png\\|300]] |", "assets/new.png", "| ![[assets/new.png\\|300]] |", 1),
            ("| ![[assets/old.png\\|300]] |", "media/old.png", "| ![[media/old.png\\|300]] |", 1),
            ("| ![[old.png\\|300]] |", "media/old.png", "| ![[old.png\\|300]] |", 0),
            ("| ![[old.png#page=2\\|300]] |", "assets/new.png", "| ![[new.png#page=2\\|300]] |", 1),
            ("| [[old.png\\|the shot]] |", "assets/new.png", "| [[new.png\\|the shot]] |", 1),
            ("![[old.png|300]]", "assets/new.png", "![[new.png|300]]", 1),
            ("`![[old.png\\|300]]`", "assets/new.png", "`![[old.png\\|300]]`", 0),
            ("| ![[other.png\\|300]] |", "assets/new.png", "| ![[other.png\\|300]] |", 0),
        ];
        for (body, new, want, changed) in cases {
            assert_eq!(rename_to(body, "assets/old.png", new), done(want, changed), "{body} -> {new}");
        }
    }

    #[test]
    fn move_re_roots_vault_root_links_keeping_a_leading_slash() {
        assert_eq!(
            mv("![[assets/old.png]] [[assets/old.png|the shot]] [p](/assets/old.png#page=2)").body,
            "![[media/shots/old.png]] [[media/shots/old.png|the shot]] [p](/media/shots/old.png#page=2)"
        );
    }

    #[test]
    fn move_keeps_a_note_relative_href_relative_to_the_note() {
        assert_eq!(mv("[open](../../assets/old.png \"Title\")").body, "[open](../../media/shots/old.png \"Title\")");
        // A note at the vault root writes the plain path either way.
        assert_eq!(rw("![](assets/old.png)", &ASSETS, "Root.md", "assets/old.png", "media/old.png").body, "![](media/old.png)");
        // Moving into the note's own folder yields a bare relative name.
        assert_eq!(
            rw("![](../../assets/old.png)", &ASSETS, NOTE, "assets/old.png", "inbox/Daily/old.png").body,
            "![](old.png)"
        );
    }

    #[test]
    fn move_leaves_a_bare_name_bare_while_it_still_names_one_asset() {
        assert_eq!(mv("![[old.png]] ![](old.png)"), done("![[old.png]] ![](old.png)", 0));
    }

    #[test]
    fn move_spells_out_the_path_when_the_bare_name_would_become_ambiguous() {
        assert_eq!(
            rename_to("![[old.png]] ![[assets/old.png]]", "assets/old.png", "docs/dup.png").body,
            "![[docs/dup.png]] ![[docs/dup.png]]"
        );
        assert_eq!(rename_to("![[old.png]]", "assets/old.png", "assets/dup.png").body, "![[assets/dup.png]]");
    }

    #[test]
    fn move_of_an_asset_next_to_its_note() {
        let local = ["inbox/pic.png", "inbox/sub/chart.png", "assets/other.png"];
        let body = "![[pic.png]] ![alt](pic.png) [[inbox/pic.png|the pic]] ![[assets/other.png]]";
        assert_eq!(
            rw(body, &local, "inbox/Pics.md", "inbox/pic.png", "media/shots/pic.png").body,
            "![[pic.png]] ![alt](../media/shots/pic.png) [[media/shots/pic.png|the pic]] ![[assets/other.png]]"
        );
        // A wikilink with a note-relative folder in it becomes a vault-root
        // path, never `../`.
        assert_eq!(
            rw("![[sub/chart.png]] ![](sub/chart.png)", &local, "inbox/Pics.md", "inbox/sub/chart.png", "media/chart.png").body,
            "![[media/chart.png]] ![](../media/chart.png)"
        );
        // A bare wikilink whose name stops being unique spells out the path.
        assert_eq!(
            rw("![[pic.png]]", &local, "inbox/Pics.md", "inbox/pic.png", "assets/other.png").body,
            "![[assets/other.png]]"
        );
    }

    #[test]
    fn move_keeps_encoding_angle_brackets_and_code() {
        let out = rename_to(
            "![](assets/old%20name.png) ![](<assets/old name.png>) `![[assets/old name.png]]`",
            "assets/old name.png",
            "media/new name.png",
        );
        assert_eq!(
            out,
            done("![](media/new%20name.png) ![](<media/new name.png>) `![[assets/old name.png]]`", 2)
        );
    }

    const PDFS: [&str; 2] = ["assets/report.pdf", "assets/other.pdf"];

    #[test]
    fn a_markdown_link_whose_file_gains_a_space_or_a_parenthesis() {
        let cases: [(&str, &str, &str); 9] = [
            ("[x](../assets/report.pdf)\n", "assets/new report.pdf", "[x](<../assets/new report.pdf>)\n"),
            ("[x](../assets/report.pdf#page=3)\n", "assets/new report.pdf", "[x](<../assets/new report.pdf#page=3>)\n"),
            ("[x](../assets/report.pdf \"T\")\n", "assets/new report.pdf", "[x](<../assets/new report.pdf> \"T\")\n"),
            ("![x](../assets/report.pdf)\n", "assets/new report.pdf", "![x](<../assets/new report.pdf>)\n"),
            ("[x](../assets/report.pdf)\n", "assets/report(1).pdf", "[x](<../assets/report(1).pdf>)\n"),
            ("[x](../assets/report.pdf)\n", "assets/renamed.pdf", "[x](../assets/renamed.pdf)\n"),
            ("[x](<../assets/report.pdf>)\n", "assets/new report.pdf", "[x](<../assets/new report.pdf>)\n"),
            ("[x](<../assets/report.pdf>)\n", "assets/a>b.pdf", "[x](<../assets/a%3Eb.pdf>)\n"),
            ("![[assets/report.pdf]]\n", "assets/new report.pdf", "![[assets/new report.pdf]]\n"),
        ];
        for (body, new, want) in cases {
            assert_eq!(rw(body, &PDFS, "notes/n.md", "assets/report.pdf", new).body, want, "{body} -> {new}");
        }
    }

    #[test]
    fn stays_encoded_when_it_was_and_the_next_rename_finds_it_again() {
        assert_eq!(
            rw("[x](../assets/my%20report.pdf)\n", &["assets/my report.pdf"], "notes/n.md", "assets/my report.pdf", "assets/new report.pdf").body,
            "[x](../assets/new%20report.pdf)\n"
        );
        let first = rw("[x](../assets/report.pdf#page=3)\n", &PDFS, "notes/n.md", "assets/report.pdf", "assets/new report.pdf");
        let second = rw(&first.body, &["assets/new report.pdf"], "notes/n.md", "assets/new report.pdf", "assets/final.pdf");
        assert_eq!(second, done("[x](<../assets/final.pdf#page=3>)\n", 1));
    }

    #[test]
    fn leaves_a_wikilink_that_names_a_note_when_told_the_notes() {
        let assets = refs(&["Docs/Plan"]);
        let notes = [NoteRef { path: "Plan.md", title: "Plan", folder: "inbox" }];
        let body = "See [[Plan]] and [[Plan|the plan]] and ![[Docs/Plan]].\n";
        assert_eq!(
            rewrite_asset_references(body, &assets, "N.md", "Docs/Plan", "Docs/Other", &notes, false).body,
            "See [[Plan]] and [[Plan|the plan]] and ![[Docs/Other]].\n"
        );
        assert_eq!(rewrite_asset_references(body, &assets, "N.md", "Docs/Plan", "Docs/Other", &[], false).changed, 3);
        // A note in the trash names nothing.
        let trashed = [NoteRef { path: "trash/Plan.md", title: "Plan", folder: "trash" }];
        assert_eq!(rewrite_asset_references(body, &assets, "N.md", "Docs/Plan", "Docs/Other", &trashed, false).changed, 3);
    }

    #[test]
    fn writes_the_whole_path_when_asked_however_unique_the_bare_name_is() {
        let assets = refs(&["assets/a.png"]);
        let body = "![[a.png]]\n";
        assert_eq!(rewrite_asset_references(body, &assets, "N.md", "assets/a.png", "assets/b.png", &[], false).body, "![[b.png]]\n");
        assert_eq!(
            rewrite_asset_references(body, &assets, "N.md", "assets/a.png", "assets/b.png", &[], true).body,
            "![[assets/b.png]]\n"
        );
    }

    #[test]
    fn reads_through_an_anchor_when_it_asks_whether_a_wikilink_names_a_note() {
        let assets = refs(&["Docs/Report"]);
        let notes = [NoteRef { path: "Report.md", title: "Report", folder: "inbox" }];
        let body = "[[Report#Heading]] [[Report^block]] [[Report|alias]]\n";
        assert_eq!(rewrite_asset_references(body, &assets, "N.md", "Docs/Report", "Docs/Renamed", &notes, false).body, body);
    }

    #[test]
    fn says_when_a_wikilink_cannot_hold_the_new_path() {
        let out = rw("![[assets/a.png]] ![x](assets/a.png)\n", &["assets/a.png", "Other/a.png"], "N.md", "assets/a.png", "Issue #5/a.png");
        assert!(out.stranded);
        assert_eq!(out.body, "![[assets/a.png]] ![x](<Issue %235/a.png>)\n");
        assert_eq!(out.changed, 1);
    }

    #[test]
    fn uses_a_bare_name_in_a_wikilink_where_that_resolves() {
        assert_eq!(rw("![[assets/a.png]]\n", &["assets/a.png"], "N.md", "assets/a.png", "a?b/a.png"), done("![[a.png]]\n", 1));
    }

    #[test]
    fn cannot_use_a_bare_name_that_holds_link_syntax_itself() {
        let out = rw("![[a.png]]\n", &["a.png"], "N.md", "a.png", "x#y.png");
        assert_eq!(out, AssetLinkRewrite { body: "![[a.png]]\n".into(), changed: 0, stranded: true });
    }

    /* the MCP tools' own cases (asset-work-tools.test.ts) -------------- */

    #[test]
    fn rename_then_move_keeps_every_kind_of_link() {
        let body = "![[report.pdf]] [x](../assets/report.pdf) [y](<../assets/report.pdf>) `[z](../assets/report.pdf)`\n";
        let first = rw(body, &["assets/report.pdf"], "notes/N.md", "assets/report.pdf", "assets/new report.pdf");
        assert_eq!(
            first,
            done("![[new report.pdf]] [x](<../assets/new report.pdf>) [y](<../assets/new report.pdf>) `[z](../assets/report.pdf)`\n", 3)
        );
        let second = rw(&first.body, &["assets/new report.pdf"], "notes/N.md", "assets/new report.pdf", "files/new report.pdf");
        assert_eq!(
            second,
            done("![[new report.pdf]] [x](<../files/new report.pdf>) [y](<../files/new report.pdf>) `[z](../assets/report.pdf)`\n", 2)
        );
    }

    #[test]
    fn a_scoped_move_writes_the_full_path() {
        let notes = [NoteRef { path: "Projects/P.md", title: "P", folder: "inbox" }];
        let out = rewrite_asset_references("![[assets/a.png]]\n", &refs(&["assets/a.png"]), "Projects/P.md", "assets/a.png", "Projects/Files/a.png", &notes, true);
        assert_eq!(out, done("![[Projects/Files/a.png]]\n", 1));
        let out = rewrite_asset_references(&out.body, &refs(&["Projects/Files/a.png"]), "Projects/P.md", "Projects/Files/a.png", "Projects/Files/b.png", &notes, true);
        assert_eq!(out, done("![[Projects/Files/b.png]]\n", 1));
    }

    /* more edges ------------------------------------------------------- */

    #[test]
    fn relative_paths_from_a_note_in_a_subfolder() {
        let assets = ["Projects/Alpha/img/a.png", "Projects/shared.png"];
        let note = "Projects/Alpha/Plan.md";
        assert_eq!(
            rw("![](img/a.png) ![](./img/a.png) ![](../Alpha/img/a.png)", &assets, note, "Projects/Alpha/img/a.png", "Projects/Beta/a.png").body,
            "![](../Beta/a.png) ![](../Beta/a.png) ![](../Beta/a.png)"
        );
        assert_eq!(
            rw("![](../shared.png)", &assets, note, "Projects/shared.png", "Projects/Alpha/shared.png").body,
            "![](shared.png)"
        );
    }

    #[test]
    fn fenced_and_inline_code_with_crlf_and_tildes_stays() {
        let body = "```js\r\n![[assets/old.png]]\r\n```\r\n~~~\r\n[x](assets/old.png)\r\n~~~\r\n![[assets/old.png]]\r\n";
        assert_eq!(
            rename(body),
            done("```js\r\n![[assets/old.png]]\r\n```\r\n~~~\r\n[x](assets/old.png)\r\n~~~\r\n![[assets/new.png]]\r\n", 1)
        );
        // An unclosed fence is not code: two backticks are an empty span.
        assert_eq!(rename("```\n![[assets/old.png]]").body, "```\n![[assets/new.png]]");
    }

    #[test]
    fn spacing_inside_a_destination_is_dropped_on_rewrite() {
        assert_eq!(rename("[x]( assets/old.png )"), done("[x](assets/new.png)", 1));
        assert_eq!(rename("[x](assets/old.png\t'T')"), done("[x](assets/new.png\t'T')", 1));
        assert_eq!(rename("[x](assets/old.png (T))"), done("[x](assets/new.png (T))", 1));
    }

    #[test]
    fn a_malformed_escape_reads_as_written() {
        let assets = ["assets/50%.png"];
        assert_eq!(rw("![](assets/50%.png)", &assets, "N.md", "assets/50%.png", "assets/60%.png").body, "![](assets/60%.png)");
        // Encoded once, so the new name is encoded (and its `%` with it).
        let assets = ["assets/a b.png"];
        assert_eq!(rw("![](assets/a%20b.png)", &assets, "N.md", "assets/a b.png", "assets/50% b.png").body, "![](assets/50%25%20b.png)");
    }

    #[test]
    fn non_ascii_names_encode_like_the_author() {
        let assets = ["assets/café.png"];
        assert_eq!(rw("![](assets/caf%C3%A9.png) ![[assets/café.png]]", &assets, "N.md", "assets/café.png", "assets/thé.png").body, "![](assets/th%C3%A9.png) ![[assets/thé.png]]");
        assert_eq!(rw("![](assets/café.png)", &assets, "N.md", "assets/café.png", "assets/thé.png").body, "![](assets/thé.png)");
    }

    /* asset-path-resolution.ts (local-assets-vault-root.test.ts) ------- */

    #[test]
    fn vault_root_wikilink_embeds_resolve_from_the_root() {
        let daily = "Daily Notes/2026-07-24.md";
        let both = refs(&["assets/image.png", "Photos/image.png"]);
        assert_eq!(resolve_asset_path_among(&both, daily, "assets/image.png").as_deref(), Some("assets/image.png"));
        assert_eq!(resolve_asset_path_among(&both, daily, "Photos/image.png").as_deref(), Some("Photos/image.png"));
        assert_eq!(resolve_asset_path_among(&both, daily, "/assets/image.png").as_deref(), Some("assets/image.png"));
        let one = refs(&["assets/image.png"]);
        assert_eq!(resolve_asset_path_among(&one, daily, "assets/image.png").as_deref(), Some("assets/image.png"));
        let sib = refs(&["Folder/pic.png", "pic.png"]);
        assert_eq!(resolve_asset_path_among(&sib, "Folder/note.md", "pic.png").as_deref(), Some("Folder/pic.png"));
    }

    #[test]
    fn obsidian_import_per_folder_attachments() {
        let vault = refs(&[
            "ProjectA/attachments/imageA1.png",
            "ProjectB/attachments/imageA1.png",
            "assets/image.png",
            "attachments/Pasted image 20260610101407.png",
        ]);
        assert_eq!(
            resolve_asset_path_among(&vault, "ProjectA/fileA1.md", "ProjectA/attachments/imageA1.png").as_deref(),
            Some("ProjectA/attachments/imageA1.png")
        );
        assert_eq!(
            resolve_asset_path_among(&vault, "ProjectB/fileB1.md", "ProjectB/attachments/imageA1.png").as_deref(),
            Some("ProjectB/attachments/imageA1.png")
        );
        assert_eq!(
            resolve_asset_path_among(&vault, "ProjectA/fileA1.md", "/ProjectA/attachments/imageA1.png").as_deref(),
            Some("ProjectA/attachments/imageA1.png")
        );
    }

    #[test]
    fn resolution_readings_and_refusals() {
        let assets = refs(&["assets/a b.png", "inbox/x.png", "Dup/d.png", "Other/D.PNG"]);
        let r = resolve_asset_reference(&assets, "inbox/N.md", "x.png").unwrap();
        assert_eq!((r.path.as_str(), r.reading, r.absolute), ("inbox/x.png", AssetReferenceReading::NoteRelative, false));
        let r = resolve_asset_reference(&assets, "N.md", "assets/a%20b.png?raw#p").unwrap();
        assert_eq!((r.path.as_str(), r.reading), ("assets/a b.png", AssetReferenceReading::VaultRoot));
        let r = resolve_asset_reference(&assets, "inbox/N.md", "/assets/a b.png").unwrap();
        assert_eq!((r.reading, r.absolute), (AssetReferenceReading::VaultRoot, true));
        let r = resolve_asset_reference(&assets, "deep/er/N.md", "A B.PNG").unwrap();
        assert_eq!((r.path.as_str(), r.reading), ("assets/a b.png", AssetReferenceReading::Basename));
        // Case-insensitive basenames collide.
        assert_eq!(resolve_asset_reference(&assets, "N.md", "d.png"), None);
        for href in ["", "  ", "#top", "//cdn/x.png", "https://x/x.png", "mailto:x.png", "C:x.png", "../x.png", "inbox/../../x.png"] {
            assert_eq!(resolve_asset_reference(&assets, "N.md", href), None, "{href:?}");
        }
        // A digit in a scheme only after the first letter.
        assert!(has_url_scheme("a1+.-:x"));
        assert!(!has_url_scheme("1a:x"));
        assert!(!has_url_scheme("ab"));
    }

    #[test]
    fn posix_helpers() {
        assert_eq!(posix_normalize("a/./b//c/../d"), "a/b/d");
        assert_eq!(posix_normalize("a/../../b"), "..");
        assert_eq!(posix_normalize("/a/"), "a");
        assert_eq!(posix_join("", "b"), "b");
        assert_eq!(posix_join("a", ""), "a");
        assert_eq!(posix_join("a/", "b"), "a/b");
        assert_eq!(posix_join("a", "b"), "a/b");
        assert_eq!(strip_query_and_hash("a?b#c"), "a");
        assert_eq!(strip_query_and_hash("a#b?c"), "a");
        assert_eq!(decode_href_path("a%20b.png#x"), "a b.png");
        assert_eq!(decode_href_path("50%.png"), "50%.png");
        assert_eq!(relative_to("inbox/Daily", "assets/x.png"), "../../assets/x.png");
        assert_eq!(relative_to("", "assets/x.png"), "assets/x.png");
        assert_eq!(relative_to("assets", "assets/x.png"), "x.png");
    }

    #[test]
    fn uri_component_codec_follows_ecmascript() {
        assert_eq!(decode_uri_component("a%20b%2Fc").as_deref(), Some("a b/c"));
        assert_eq!(decode_uri_component("%C3%A9%e2%82%ac%F0%9F%98%80").as_deref(), Some("é€😀"));
        for bad in ["%", "%2", "%zz", "%80", "%C3", "%C3%", "%C3%28", "%C0%AF", "%ED%A0%80", "%F4%90%80%80", "%F8%80%80%80%80"] {
            assert_eq!(decode_uri_component(bad), None, "{bad}");
        }
        assert_eq!(encode_uri_component("a b/é-_.!~*'()#?%"), "a%20b%2F%C3%A9-_.!~*'()%23%3F%25");
    }

    /* wikilink-rename.ts (the resolution wikilink-rename.test.ts drives) */

    #[test]
    fn wikilink_resolution_by_title_and_path() {
        let notes = [
            NoteRef { path: "inbox/demo/Old Title.md", title: "Old Title", folder: "inbox" },
            NoteRef { path: "inbox/Other.md", title: "Other", folder: "inbox" },
        ];
        let hit = |target: &str| resolve_wikilink_target(&notes, target).map(|n| n.path);
        assert_eq!(hit("Old Title"), Some("inbox/demo/Old Title.md"));
        assert_eq!(hit(" old title.MD "), Some("inbox/demo/Old Title.md"));
        assert_eq!(hit("inbox/demo/Old Title"), Some("inbox/demo/Old Title.md"));
        assert_eq!(hit("demo/Old Title"), Some("inbox/demo/Old Title.md"));
        assert_eq!(hit("/demo/Old Title"), Some("inbox/demo/Old Title.md"));
        assert_eq!(hit("inbox/demo/Old Title.md"), Some("inbox/demo/Old Title.md"));
        // A backslash is a separator once a `.md` makes the target a path...
        assert_eq!(hit("demo\\Old Title.md"), Some("inbox/demo/Old Title.md"));
        // ...but without one the target is read as a title.
        assert_eq!(hit("demo\\Old Title"), None);
        assert_eq!(hit("Other"), Some("inbox/Other.md"));
        assert_eq!(hit("Missing"), None);
        assert_eq!(hit("/"), None);
    }

    #[test]
    fn wikilink_resolution_when_titles_collide_and_in_the_trash() {
        let dup = [
            NoteRef { path: "inbox/A.md", title: "Dup", folder: "inbox" },
            NoteRef { path: "inbox/B.md", title: "Dup", folder: "inbox" },
        ];
        // `[[Dup]]` resolves to the first match.
        assert_eq!(resolve_wikilink_target(&dup, "Dup").map(|n| n.path), Some("inbox/A.md"));
        // A suffix path must be unique.
        let twins = [
            NoteRef { path: "inbox/x/Spec.md", title: "Spec", folder: "inbox" },
            NoteRef { path: "archive/y/x/Spec.md", title: "Spec", folder: "archive" },
        ];
        assert_eq!(resolve_wikilink_target(&twins, "x/Spec"), None);
        assert_eq!(resolve_wikilink_target(&twins, "y/x/Spec").map(|n| n.path), Some("archive/y/x/Spec.md"));
        assert_eq!(resolve_wikilink_target(&twins, "archive/y/x/Spec").map(|n| n.path), Some("archive/y/x/Spec.md"));
        let trash = [NoteRef { path: "trash/T.md", title: "T", folder: "trash" }];
        assert_eq!(resolve_wikilink_target(&trash, "T"), None);
        assert_eq!(resolve_wikilink_target(&trash, "trash/T"), None);
    }

    #[test]
    fn wikilink_alias_start_reads_a_table_escape() {
        assert_eq!(wikilink_alias_start("Old Title|d"), Some(9));
        assert_eq!(wikilink_alias_start("Old Title\\|d"), Some(9));
        assert_eq!(wikilink_alias_start("|d"), Some(0));
        assert_eq!(wikilink_alias_start("Old Title"), None);
        assert!(is_path_like_wikilink_target(" /x "));
        assert!(is_path_like_wikilink_target("a.MD"));
        assert!(!is_path_like_wikilink_target("a\\b"));
    }
}
