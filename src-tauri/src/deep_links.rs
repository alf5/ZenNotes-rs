//! Deep links (`synnotes://`) + file-open routing — port of
//! apps/desktop/src/main/deep-links.ts (rebranded scheme).

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

pub const DEEP_LINK_SCHEME: &str = "synnotes";

#[derive(Debug, PartialEq)]
pub enum DeepLinkTarget {
    Tab,
    Window,
}

pub struct OpenNoteRequest {
    pub target: DeepLinkTarget,
    pub path: String,
}

/// Port of `normalizeDeepLinkNotePath` — reject absolute/traversal paths.
pub fn normalize_deep_link_note_path(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }
    let slash = trimmed.replace('\\', "/");
    if slash.starts_with('/') {
        return None;
    }
    // Windows drive prefix like C:/
    let bytes = slash.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/' {
        return None;
    }
    if slash.split('/').any(|p| p == "..") {
        return None;
    }
    let normalized = crate::vault::notes::normalize_vault_relative_path(&slash);
    if normalized.is_empty() || normalized == ".." || normalized.starts_with("../") {
        return None;
    }
    Some(normalized)
}

/// Port of `parseOpenNoteDeepLink`.
pub fn parse_open_note_deep_link(raw_url: &str) -> Option<OpenNoteRequest> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let prefix = format!("{DEEP_LINK_SCHEME}://");
    let rest = trimmed.strip_prefix(&prefix)?;
    // action is the host (before the first '/' or '?').
    let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let action = &rest[..host_end];
    let target = match action {
        "open" => DeepLinkTarget::Tab,
        "open-window" => DeepLinkTarget::Window,
        _ => return None,
    };
    // Extract the `path` query param.
    let query = rest.split('?').nth(1).unwrap_or("");
    let mut path_param: Option<String> = None;
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("path=") {
            path_param = Some(urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string()));
            break;
        }
    }
    let path = normalize_deep_link_note_path(path_param.as_deref())?;
    Some(OpenNoteRequest { target, path })
}

/// Open a note from a deep link: floating window, or tab in the main window
/// (queued if the renderer hasn't signalled ready yet).
pub fn open_note_request(app: &AppHandle, req: OpenNoteRequest) {
    match req.target {
        DeepLinkTarget::Window => {
            let _ = crate::windows::open_note_window(app, &req.path);
        }
        DeepLinkTarget::Tab => {
            let state = app.state::<AppState>();
            if state.is_renderer_ready() {
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.emit("app://open-note", req.path);
                    let _ = main.set_focus();
                    return;
                }
            }
            state.queue_open_note(req.path);
        }
    }
}

/// Route a raw URL/file argument: a `synnotes://` deep link, a `file://` URL,
/// or a plain path. Returns true when handled.
pub fn handle_url_or_path(app: &AppHandle, raw: &str) -> bool {
    if raw.starts_with(&format!("{DEEP_LINK_SCHEME}://")) {
        if let Some(req) = parse_open_note_deep_link(raw) {
            open_note_request(app, req);
            return true;
        }
        return false;
    }
    let path = if let Some(rest) = raw.strip_prefix("file://") {
        urlencoding::decode(rest).map(|c| c.into_owned()).unwrap_or_else(|_| rest.to_string())
    } else {
        raw.to_string()
    };
    if path.to_lowercase().ends_with(".md") {
        let state = app.state::<AppState>();
        return crate::windows::open_markdown_file(app, &state, &path).unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_tab() {
        let req = parse_open_note_deep_link("synnotes://open?path=inbox/A.md").unwrap();
        assert_eq!(req.target, DeepLinkTarget::Tab);
        assert_eq!(req.path, "inbox/A.md");
    }

    #[test]
    fn parses_open_window() {
        let req = parse_open_note_deep_link("synnotes://open-window?path=inbox%2FB.md").unwrap();
        assert_eq!(req.target, DeepLinkTarget::Window);
        assert_eq!(req.path, "inbox/B.md");
    }

    #[test]
    fn rejects_bad_paths_and_actions() {
        assert!(parse_open_note_deep_link("synnotes://open?path=../escape.md").is_none());
        assert!(parse_open_note_deep_link("synnotes://open?path=/abs.md").is_none());
        assert!(parse_open_note_deep_link("synnotes://bogus?path=inbox/A.md").is_none());
        assert!(parse_open_note_deep_link("https://example.com").is_none());
    }
}
