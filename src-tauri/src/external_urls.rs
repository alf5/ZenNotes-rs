//! `openExternalUrl` — port of apps/desktop/src/main/external-urls.ts plus the
//! classifier it calls (packages/shared-domain/src/application-links.ts).
//!
//! The backend owns permission to launch an application from a note: the
//! renderer asks, and only http(s)/mailto/tel or an application scheme the
//! user enabled in Settings (`[editor] external_application_schemes` in
//! config.toml) is handed to the OS. The opener plugin grants the webview
//! nothing directly, so this command is the only way out — which is why the
//! check lives here and not in the webview, where a hostile note preview could
//! skip it. Keep the scheme tables byte-compatible with the shared-domain file.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

const STANDARD_SCHEMES: [&str; 4] = ["http", "https", "mailto", "tel"];
const RESERVED_SCHEMES: [&str; 14] = [
    "javascript",
    "vbscript",
    "data",
    "blob",
    "file",
    "filesystem",
    "about",
    "chrome",
    "chrome-extension",
    "devtools",
    "electron",
    "resource",
    "view-source",
    "zen",
];

/// Mirrors `ExternalUrlResult` in bridge-contract/src/application-links.ts.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExternalUrlResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
}

impl ExternalUrlResult {
    fn fail(error: &'static str, scheme: Option<String>) -> Self {
        Self { ok: false, error: Some(error), scheme }
    }
}

fn is_application_scheme(scheme: &str) -> bool {
    !STANDARD_SCHEMES.contains(&scheme)
        && !RESERVED_SCHEMES.contains(&scheme)
        && !scheme.starts_with("zen-")
}

fn is_scheme_text(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '.' | '-'))
}

fn has_control_or_space(url: &str) -> bool {
    url.chars().any(|c| (c as u32) <= 0x20 || c as u32 == 0x7f)
}

/// Port of `normalizeApplicationSchemes`.
pub fn normalize_application_schemes(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in values {
        let lower = entry.trim().to_lowercase();
        let scheme = lower
            .strip_suffix("://")
            .or_else(|| lower.strip_suffix(':'))
            .unwrap_or(&lower);
        if is_scheme_text(scheme) && is_application_scheme(scheme) && !out.iter().any(|s| s == scheme) {
            out.push(scheme.to_string());
        }
    }
    out
}

/// Port of `classifyApplicationLink`: (scheme, blocked) for an application
/// link, None for anything the existing handlers own.
pub fn classify_application_link(href: &str) -> Option<(String, bool)> {
    let url = href.trim();
    let bytes = url.as_bytes();
    // A Windows drive path is a file, not a one-letter application scheme.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && bytes.get(3) != Some(&b'/')
    {
        return None;
    }
    let colon = url.find(':')?;
    let scheme = url[..colon].to_lowercase();
    if !is_scheme_text(&scheme)
        || STANDARD_SCHEMES.contains(&scheme.as_str())
        || scheme == "file"
        || scheme == "zen"
        || scheme.starts_with("zen-")
    {
        return None;
    }
    let units = url.encode_utf16().count();
    let blocked = !is_application_scheme(&scheme)
        || has_control_or_space(url)
        || units > 8192
        || units == scheme.len() + 1;
    Some((scheme, blocked))
}

/// Port of `openExternalUrl`, with the opener injected for tests.
pub fn open_external_url(
    value: &str,
    enabled: &[String],
    open: impl FnOnce(&str) -> Result<(), String>,
) -> ExternalUrlResult {
    let url = value.trim();
    if url.is_empty() || url.encode_utf16().count() > 8192 || has_control_or_space(url) {
        return ExternalUrlResult::fail("blocked", None);
    }
    let application = classify_application_link(url);
    match &application {
        Some((scheme, true)) => return ExternalUrlResult::fail("blocked", Some(scheme.clone())),
        Some((scheme, false)) => {
            if !normalize_application_schemes(enabled).contains(scheme) {
                return ExternalUrlResult::fail("scheme-disabled", Some(scheme.clone()));
            }
        }
        None => {
            let lower = url.to_lowercase();
            if !["http:", "https:", "mailto:", "tel:"].iter().any(|p| lower.starts_with(p)) {
                return ExternalUrlResult::fail("blocked", None);
            }
        }
    }
    if url::Url::parse(url).is_err() {
        return ExternalUrlResult::fail("blocked", None);
    }
    match open(url) {
        Ok(()) => ExternalUrlResult { ok: true, error: None, scheme: None },
        Err(_) => ExternalUrlResult::fail("open-failed", application.map(|(scheme, _)| scheme)),
    }
}

/// The schemes enabled in config.toml, read fresh on every open: the renderer
/// writes the file whenever the setting changes, exactly like upstream's
/// portable-config snapshot in main.
fn enabled_schemes() -> Vec<String> {
    let path = crate::app_config::config_dir().join(crate::app_config::CONFIG_FILE_NAME);
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(doc) = raw.parse::<toml::Table>() else {
        return Vec::new();
    };
    doc.get("editor")
        .and_then(|editor| editor.get("external_application_schemes"))
        .and_then(toml::Value::as_array)
        .map(|list| list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[tauri::command]
pub fn app_open_external_url(app: AppHandle, url: String) -> ExternalUrlResult {
    open_external_url(&url, &enabled_schemes(), |target| {
        app.opener().open_url(target, None::<&str>).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(url: &str) -> Option<(String, bool)> {
        classify_application_link(url)
    }

    /// The cases of shared-domain application-links.test.ts.
    #[test]
    fn classifies_like_shared_domain() {
        assert_eq!(
            classify("Zotero://open-pdf/library/items/W78FUE98?page=3#annotation=ABC"),
            Some(("zotero".into(), false))
        );
        for url in ["obsidian://open?vault=Work", "vscode://file/tmp/note.md", "x://note", "things:add?title=Task"] {
            assert_eq!(classify(url).map(|c| c.1), Some(false), "{url}");
        }
        for url in [
            "https://example.com",
            "mailto:a@b.com",
            "tel:+123",
            "file:///tmp/a.pdf",
            "zen-asset://a",
            "zen://asset/a",
            "C:\\a.pdf",
            "C:/a.pdf",
            "../Note.md",
            "Note.md",
            "#Heading",
        ] {
            assert_eq!(classify(url), None, "{url}");
        }
        for url in [
            "javascript:alert(1)",
            "data:text/html,test",
            "vbscript:run",
            "about:blank",
            "chrome://settings",
            "zotero:",
            "zotero://bad\nvalue",
            "zotero://bad value",
        ] {
            assert_eq!(classify(url).map(|c| c.1), Some(true), "{url}");
        }
    }

    #[test]
    fn normalizes_enabled_schemes_like_shared_domain() {
        let input: Vec<String> = [
            " Zotero:// ", "zotero", "OBSIDIAN:", "vscode", "javascript", "file", "https", "zen-asset",
            "bad value",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(normalize_application_schemes(&input), vec!["zotero", "obsidian", "vscode"]);
    }

    #[test]
    fn opens_only_what_is_allowed() {
        let enabled = vec!["zotero".to_string()];
        let opened = |url: &str| -> ExternalUrlResult {
            open_external_url(url, &enabled, |_| Ok(()))
        };
        assert!(opened("https://example.com").ok);
        assert!(opened("zotero://select/items/1").ok);
        assert_eq!(
            opened("obsidian://open"),
            ExternalUrlResult::fail("scheme-disabled", Some("obsidian".into()))
        );
        assert_eq!(opened("javascript:alert(1)").error, Some("blocked"));
        // Any other scheme is an application scheme, off until enabled.
        assert_eq!(opened("ftp://x").error, Some("scheme-disabled"));
        assert_eq!(opened("notaurl").error, Some("blocked"));
        assert_eq!(opened("https://exa mple.com").error, Some("blocked"));
        let failed = open_external_url("zotero://x", &enabled, |_| Err("no handler".into()));
        assert_eq!(failed, ExternalUrlResult::fail("open-failed", Some("zotero".into())));
    }
}
