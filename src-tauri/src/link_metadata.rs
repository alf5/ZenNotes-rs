//! Open-graph metadata for Notion-style bookmark cards — Rust port of
//! upstream `apps/desktop/src/main/link-metadata.ts`.
//!
//! Guards (upstream parity): https only; obvious loopback / private hosts are
//! refused, both before the request and after redirects (a note is untrusted
//! content, so it must not point the fetch at internal services); a hard
//! 6 s timeout; and the body is read only up to 512 KB (metadata lives in
//! `<head>`). Never errors — any failure returns `{url, ok: false}`.

use std::io::Read;
use std::sync::LazyLock;
use std::time::Duration;

use fancy_regex::Regex;
use url::Url;

use crate::ipc::types::LinkMetadata;

const TIMEOUT_MS: u64 = 6000;
const MAX_BYTES: u64 = 512 * 1024;
const USER_AGENT: &str =
    "Mozilla/5.0 (compatible; ZenNotes/1.0; +https://zennotes.app) LinkPreview";

static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<title[^>]*>([^<]*)</title>").unwrap());
static LINK_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<link[^>]+>").unwrap());
static ICON_REL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)rel=["'][^"']*icon[^"']*["']"#).unwrap());
static HREF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)href=["']([^"']+)["']"#).unwrap());
static CONTENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)content=["']([^"']*)["']"#).unwrap());

fn is_blocked_host(hostname: &str) -> bool {
    let h = hostname.to_lowercase();
    let h = h.trim_start_matches('[').trim_end_matches(']');
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    if h == "::1" || h == "0.0.0.0" {
        return true;
    }
    // IPv4 private / loopback / link-local ranges.
    if h.starts_with("127.") || h.starts_with("10.") || h.starts_with("192.168.") || h.starts_with("169.254.") {
        return true;
    }
    if let Some(rest) = h.strip_prefix("172.") {
        if let Some((octet, _)) = rest.split_once('.') {
            if let Ok(n) = octet.parse::<u8>() {
                if (16..=31).contains(&n) {
                    return true;
                }
            }
        }
    }
    false
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&#x2F;", "/")
        .replace("&#x2f;", "/")
        .replace("&nbsp;", " ")
}

/// Read a `<meta property=".." content="..">` (or `name=`), order-agnostic.
fn meta_tag(html: &str, key: &str) -> Option<String> {
    let escaped = fancy_regex::escape(key);
    let re = Regex::new(&format!(
        r#"(?i)<meta[^>]+(?:property|name)=["']{escaped}["'][^>]*>"#
    ))
    .ok()?;
    let tag = re.find(html).ok().flatten()?.as_str();
    let content = CONTENT_RE
        .captures(tag)
        .ok()
        .flatten()
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())?;
    let decoded = decode_entities(content).trim().to_string();
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn absolute(value: Option<String>, base: &Url) -> Option<String> {
    value.and_then(|v| base.join(&v).ok().map(String::from))
}

fn first_favicon(html: &str, base: &Url) -> Option<String> {
    for tag in LINK_TAG_RE.find_iter(html).flatten() {
        let tag = tag.as_str();
        if !ICON_REL_RE.is_match(tag).unwrap_or(false) {
            continue;
        }
        if let Some(href) = HREF_RE
            .captures(tag)
            .ok()
            .flatten()
            .and_then(|c| c.get(1))
            .map(|m| decode_entities(m.as_str()))
        {
            if let Ok(joined) = base.join(&href) {
                return Some(joined.into());
            }
        }
    }
    base.join("/favicon.ico").ok().map(String::from)
}

/// Pure extraction over fetched HTML — separated for tests.
fn parse_metadata(requested_url: &str, html: &str, final_url: &Url) -> LinkMetadata {
    let title_tag = TITLE_RE
        .captures(html)
        .ok()
        .flatten()
        .and_then(|c| c.get(1))
        .map(|m| decode_entities(m.as_str()).trim().to_string())
        .filter(|t| !t.is_empty());
    let title = meta_tag(html, "og:title")
        .or_else(|| meta_tag(html, "twitter:title"))
        .or(title_tag);
    let description = meta_tag(html, "og:description")
        .or_else(|| meta_tag(html, "twitter:description"))
        .or_else(|| meta_tag(html, "description"));
    let image = absolute(
        meta_tag(html, "og:image").or_else(|| meta_tag(html, "twitter:image")),
        final_url,
    );
    let site_name = meta_tag(html, "og:site_name").or_else(|| {
        final_url
            .host_str()
            .map(|h| h.trim_start_matches("www.").to_string())
    });

    LinkMetadata {
        url: requested_url.to_string(),
        ok: true,
        title,
        description,
        image,
        favicon: first_favicon(html, final_url),
        site_name,
    }
}

pub fn fetch_link_metadata(raw_url: &str) -> LinkMetadata {
    let url = raw_url.trim().to_string();
    let fail = LinkMetadata::fail(url.clone());
    let Ok(parsed) = Url::parse(&url) else {
        return fail;
    };
    if parsed.scheme() != "https" || parsed.host_str().is_none_or(is_blocked_host) {
        return fail;
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(TIMEOUT_MS))
        .user_agent(USER_AGENT)
        .build();
    let Ok(response) = agent
        .get(parsed.as_str())
        .set("accept", "text/html,application/xhtml+xml")
        .call()
    else {
        return fail;
    };
    // Re-check the post-redirect host (redirect-based SSRF protection).
    let Ok(final_url) = Url::parse(response.get_url()) else {
        return fail;
    };
    if final_url.host_str().is_none_or(is_blocked_host) {
        return fail;
    }

    let mut html_bytes = Vec::new();
    if response
        .into_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut html_bytes)
        .is_err()
    {
        return fail;
    }
    let html = String::from_utf8_lossy(&html_bytes);
    parse_metadata(&url, &html, &final_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_hosts() {
        for h in ["localhost", "api.localhost", "127.0.0.1", "10.1.2.3", "192.168.0.10", "169.254.1.1", "172.16.0.1", "172.31.9.9", "0.0.0.0", "::1"] {
            assert!(is_blocked_host(h), "{h} should be blocked");
        }
        for h in ["example.com", "172.15.0.1", "172.32.0.1", "8.8.8.8"] {
            assert!(!is_blocked_host(h), "{h} should be allowed");
        }
    }

    #[test]
    fn parses_meta_order_agnostic_with_entities() {
        let base = Url::parse("https://example.com/a/b").unwrap();
        let html = r#"<head>
          <title>Fallback &amp; title</title>
          <meta content="OG &quot;Title&quot;" property="og:title">
          <meta name="description" content="Some description">
          <meta property="og:image" content="/img/card.png">
          <link rel="shortcut icon" href="/fav.ico">
        </head>"#;
        let meta = parse_metadata("https://example.com/a/b", html, &base);
        assert!(meta.ok);
        assert_eq!(meta.title.as_deref(), Some(r#"OG "Title""#));
        assert_eq!(meta.description.as_deref(), Some("Some description"));
        assert_eq!(meta.image.as_deref(), Some("https://example.com/img/card.png"));
        assert_eq!(meta.favicon.as_deref(), Some("https://example.com/fav.ico"));
        assert_eq!(meta.site_name.as_deref(), Some("example.com"));
    }

    /// Real-network check — run explicitly: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_example_dot_com() {
        let meta = fetch_link_metadata("https://example.com/");
        assert!(meta.ok, "fetch failed");
        assert_eq!(meta.title.as_deref(), Some("Example Domain"));
        // http and private hosts refused.
        assert!(!fetch_link_metadata("http://example.com/").ok);
        assert!(!fetch_link_metadata("https://127.0.0.1/x").ok);
    }

    #[test]
    fn favicon_falls_back_to_origin() {
        let base = Url::parse("https://www.example.com/deep/page").unwrap();
        let meta = parse_metadata("https://www.example.com/deep/page", "<title>t</title>", &base);
        assert_eq!(meta.favicon.as_deref(), Some("https://www.example.com/favicon.ico"));
        assert_eq!(meta.site_name.as_deref(), Some("example.com"));
    }
}
