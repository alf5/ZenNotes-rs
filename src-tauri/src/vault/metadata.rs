//! Note metadata extraction — a faithful port of the regex-driven helpers in
//! apps/desktop/src/main/vault.ts (`stripCodeContent`, `extractTags`,
//! `extractWikilinks`, `bodyHasLocalAsset`, `buildExcerpt`).
//!
//! Uses `fancy-regex` (supports lookahead, lazy quantifiers and `[\s\S]`) so
//! the JS patterns translate 1:1. Character classes are spelled out as ASCII
//! ranges where the JS source relied on `\w` being ASCII (no `u` flag).

use std::collections::BTreeSet;
use std::sync::LazyLock;

use fancy_regex::Regex;

const IMAGE_EXTENSIONS: &[&str] =
    &[".apng", ".avif", ".gif", ".jpeg", ".jpg", ".png", ".svg", ".webp"];
const PDF_EXTENSIONS: &[&str] = &[".pdf"];
const AUDIO_EXTENSIONS: &[&str] = &[".aac", ".flac", ".m4a", ".mp3", ".ogg", ".wav"];
const VIDEO_EXTENSIONS: &[&str] = &[".m4v", ".mov", ".mp4", ".ogv", ".webm"];

static FENCED_CODE_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\n)```[^\n]*\n[\s\S]*?\n```[ \t]*(?=\n|$)").unwrap());
static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`\n]*`").unwrap());
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)#([a-zA-Z][A-Za-z0-9_\-/]*)").unwrap());
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(!?)\[[^\]]*\]\((<[^>]+>|[^)\s]+)(?:\s+"[^"]*")?\)"#).unwrap()
});
static EMBED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[\[([^\]|]+?)(?:\|[^\]]+)?\]\]").unwrap());
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(!?)\[\[([^\]|]+?)(?:\|[^\]]+)?\]\]").unwrap());
static FRONTMATTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---\n[\s\S]*?\n---\n").unwrap());
static MD_IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap());
static ASSET_MD_EMBED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\(\s*<?([^)>\s]+)>?[^)]*\)").unwrap());
static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap());
static EMBED_LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").unwrap());
static WIKI_LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").unwrap());
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#{1,6}\s+").unwrap());
static EMPHASIS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[*_~>]+").unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Classify an imported asset by extension. Mirrors `classifyImportedAsset`
/// (always returns a kind; unknown extensions are "file").
pub fn classify_imported_asset(filename: &str) -> &'static str {
    let ext = match filename.rfind('.') {
        Some(idx) => filename[idx..].to_lowercase(),
        None => return "file",
    };
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        "image"
    } else if PDF_EXTENSIONS.contains(&ext.as_str()) {
        "pdf"
    } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        "audio"
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        "video"
    } else {
        "file"
    }
}

/// Image extension for a pasted clipboard item, from a suggested name or MIME.
pub fn pasted_image_extension(mime_type: &str, suggested_name: Option<&str>) -> Option<String> {
    if let Some(name) = suggested_name {
        if let Some(idx) = name.rfind('.') {
            let ext = name[idx..].to_lowercase();
            if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                return Some(ext);
            }
        }
    }
    let mime = mime_type.to_lowercase();
    let mapped = match mime.as_str() {
        "image/apng" => Some(".apng"),
        "image/avif" => Some(".avif"),
        "image/gif" => Some(".gif"),
        "image/jpeg" | "image/jpg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/svg+xml" => Some(".svg"),
        "image/webp" => Some(".webp"),
        _ => None,
    };
    if let Some(m) = mapped {
        return Some(m.to_string());
    }
    if mime.starts_with("image/") {
        return Some(".png".to_string());
    }
    None
}

/// Asset kind for a local target, or `None` if its extension isn't a known
/// asset type. Mirrors `localAssetTargetKind`.
pub fn local_asset_target_kind(target: &str) -> Option<&'static str> {
    let clean = target.split('#').next().unwrap_or(target);
    let clean = clean.split('?').next().unwrap_or(clean);
    let last_dot = clean.rfind('.')?;
    let ext = clean[last_dot..].to_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        Some("image")
    } else if PDF_EXTENSIONS.contains(&ext.as_str()) {
        Some("pdf")
    } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        Some("audio")
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        Some("video")
    } else {
        Some("file")
    }
}

/// Blank out fenced + inline code so metadata scans ignore code spans.
pub fn strip_code_content(body: &str) -> String {
    if !body.contains('`') {
        return body.to_string();
    }
    let no_fences = FENCED_CODE_BLOCK_RE.replace_all(body, "$1 ");
    INLINE_CODE_RE.replace_all(&no_fences, " ").into_owned()
}

/// Unique `#tags`, insertion-order preserved, ignoring code.
pub fn extract_tags(body: &str) -> Vec<String> {
    if !body.contains('#') {
        return Vec::new();
    }
    let stripped = strip_code_content(body);
    let mut seen: Vec<String> = Vec::new();
    let mut set: BTreeSet<String> = BTreeSet::new();
    for caps in TAG_RE.captures_iter(&stripped).flatten() {
        if let Some(m) = caps.get(1) {
            let tag = m.as_str().to_string();
            if set.insert(tag.clone()) {
                seen.push(tag);
            }
        }
    }
    seen
}

/// Unique `[[wikilink]]` targets, ignoring `![[asset embeds]]` and code.
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    if !body.contains("[[") {
        return Vec::new();
    }
    let stripped = strip_code_content(body);
    let mut seen: Vec<String> = Vec::new();
    let mut set: BTreeSet<String> = BTreeSet::new();
    for caps in WIKILINK_RE.captures_iter(&stripped).flatten() {
        let bang = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let target = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
        if target.is_empty() {
            continue;
        }
        if bang == "!" && local_asset_target_kind(target).is_some() {
            continue;
        }
        let t = target.to_string();
        if set.insert(t.clone()) {
            seen.push(t);
        }
    }
    seen
}

/// Local files embedded in the note, mirroring upstream `extractAssetEmbeds`
/// (vault.ts:1693): `![[file.png]]` wiki embeds whose target is asset-like,
/// plus `![](path)` markdown image embeds with a relative (non-URL, non-anchor)
/// target, URL-decoded. Unique, insertion-ordered.
pub fn extract_asset_embeds(body: &str) -> Vec<String> {
    let stripped = strip_code_content(body);
    let mut seen: Vec<String> = Vec::new();
    let mut set: BTreeSet<String> = BTreeSet::new();
    if stripped.contains("![[") {
        for caps in EMBED_RE.captures_iter(&stripped).flatten() {
            let target = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if !target.is_empty()
                && local_asset_target_kind(target).is_some()
                && set.insert(target.to_string())
            {
                seen.push(target.to_string());
            }
        }
    }
    if stripped.contains("](") {
        for caps in ASSET_MD_EMBED_RE.captures_iter(&stripped).flatten() {
            let raw = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if raw.is_empty() || raw.starts_with('#') || is_uri_scheme(raw) {
                continue;
            }
            let decoded = urlencoding::decode(raw)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| raw.to_string());
            if set.insert(decoded.clone()) {
                seen.push(decoded);
            }
        }
    }
    seen
}

/// Whether the body references at least one local asset (sidebar paperclip).
pub fn body_has_local_asset(body: &str) -> bool {
    if !body.contains("](") && !body.contains("![[") {
        return false;
    }
    let stripped = strip_code_content(body);
    for caps in LINK_RE.captures_iter(&stripped).flatten() {
        let mut href = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("").to_string();
        if href.starts_with('<') && href.ends_with('>') {
            href = href[1..href.len() - 1].to_string();
        }
        if href.is_empty() || href.starts_with('#') || href.starts_with("//") {
            continue;
        }
        if is_uri_scheme(&href) {
            continue;
        }
        if local_asset_target_kind(&href).is_some() {
            return true;
        }
    }
    for caps in EMBED_RE.captures_iter(&stripped).flatten() {
        let target = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if local_asset_target_kind(target).is_some() {
            return true;
        }
    }
    false
}

/// `/^[a-zA-Z][a-zA-Z\d+.-]*:/` — a URL scheme prefix (http:, mailto:, …).
fn is_uri_scheme(href: &str) -> bool {
    let bytes = href.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b':' {
            return true;
        }
        if c.is_ascii_alphanumeric() || c == b'+' || c == b'.' || c == b'-' {
            i += 1;
        } else {
            return false;
        }
    }
    false
}

fn label_or_target(caps: &fancy_regex::Captures) -> String {
    let label = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    if !label.is_empty() {
        label.to_string()
    } else {
        caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string()
    }
}

/// Short plaintext preview (≤220 chars). Mirrors `buildExcerpt`.
pub fn build_excerpt(body: &str) -> String {
    let without_front = if body.starts_with("---\n") {
        FRONTMATTER_RE.replace(body, "").into_owned()
    } else {
        body.to_string()
    };
    let mut text = strip_code_content(&without_front);
    if text.contains("](") {
        text = MD_IMAGE_RE.replace_all(&text, " ").into_owned();
        text = MD_LINK_RE.replace_all(&text, "$1").into_owned();
    }
    if text.contains("![[") {
        text = EMBED_LABEL_RE
            .replace_all(&text, |c: &fancy_regex::Captures| label_or_target(c))
            .into_owned();
    }
    if text.contains("[[") {
        text = WIKI_LABEL_RE
            .replace_all(&text, |c: &fancy_regex::Captures| label_or_target(c))
            .into_owned();
    }
    if text.contains('#') {
        text = HEADING_RE.replace_all(&text, "").into_owned();
    }
    if text.contains(['*', '_', '~', '>']) {
        text = EMPHASIS_RE.replace_all(&text, "").into_owned();
    }
    text = WHITESPACE_RE.replace_all(&text, " ").into_owned();
    let text = text.trim();
    text.chars().take(220).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_embeds_wiki_and_md_image_targets() {
        let body = "![[pic.png|300]] and ![[Other note]] plus ![alt](docs/a%20b.pdf) \
                    ![x](https://example.com/c.png) ![y](#anchor)\n```\n![[code.png]]\n```\n![[pic.png]]";
        assert_eq!(
            extract_asset_embeds(body),
            vec!["pic.png".to_string(), "docs/a b.pdf".to_string()]
        );
    }

    #[test]
    fn tags_unique_and_ordered_skip_code() {
        let body = "intro #alpha and #beta\n```\n#incode\n```\n#alpha again `#inline`";
        assert_eq!(extract_tags(body), vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn wikilinks_drop_labels_and_skip_asset_embeds() {
        let body = "see [[Target|Label]] and [[Other]] plus ![[pic.png]] and ![[NoteEmbed]]";
        // ![[pic.png]] is an asset embed → skipped; ![[NoteEmbed]] has no asset
        // ext so it stays.
        assert_eq!(
            extract_wikilinks(body),
            vec!["Target".to_string(), "Other".to_string(), "NoteEmbed".to_string()]
        );
    }

    #[test]
    fn has_local_asset_detects_relative_image_not_urls() {
        assert!(body_has_local_asset("![](images/pic.png)"));
        assert!(body_has_local_asset("![[diagram.png]]"));
        assert!(!body_has_local_asset("[site](https://example.com)"));
        assert!(!body_has_local_asset("no links here"));
    }

    #[test]
    fn excerpt_strips_markdown_and_frontmatter() {
        let body = "---\ntitle: x\n---\n# Heading\n\nSome **bold** [link](http://x) and `code`.";
        let ex = build_excerpt(body);
        assert!(ex.starts_with("Heading Some bold link"), "got: {ex}");
        assert!(!ex.contains('#') && !ex.contains('*') && !ex.contains('`'));
    }

    #[test]
    fn excerpt_truncates_to_220_chars() {
        let body = "x".repeat(500);
        assert_eq!(build_excerpt(&body).chars().count(), 220);
    }
}
