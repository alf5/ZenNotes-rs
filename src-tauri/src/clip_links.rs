//! Clip links, `zennotes-rs://new` and `zennotes-rs://daily` (upstream v2.85,
//! #620): the contract of docs/web-clipper-link.md under the port's scheme.
//!
//! Rust only receives the link and hands it to the main window, which runs
//! the rest (src/bridge/clip-links.ts, a port of upstream's clip-links.ts),
//! because the renderer is what holds open notes and their unsaved edits.
//! The one thing that stays here is the clipboard: the page that fired the
//! link vouches for the note with its SHA-256, and the webview only ever gets
//! clipboard text that matches, never whatever was copied last.

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::deep_links::DEEP_LINK_SCHEME;
use crate::state::AppState;

const CLIP_LINK_EVENT: &str = "app://clip-link";
/// Port of `MAX_CLIP_NOTE_BYTES`.
const MAX_CLIP_NOTE_BYTES: usize = 8 * 1024 * 1024;

/// A `zennotes-rs://new` or `zennotes-rs://daily` link, well-formed or not:
/// the shape `parseClipLink` claims (any case, slashes optional).
pub fn is_clip_link(raw: &str) -> bool {
    let trimmed = raw.trim();
    let Some(scheme_end) = trimmed.find(':') else { return false };
    if !trimmed[..scheme_end].eq_ignore_ascii_case(DEEP_LINK_SCHEME) {
        return false;
    }
    let rest = trimmed[scheme_end + 1..].trim_start_matches('/');
    let action_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let action = &rest[..action_end];
    action.eq_ignore_ascii_case("new") || action.eq_ignore_ascii_case("daily")
}

/// Hand a clip link to the main window, or queue it until its renderer is
/// ready. A window hidden in the tray still takes it: a silent clip raises
/// nothing, and the renderer raises the window itself for one that is not.
pub fn deliver(app: &AppHandle, raw: &str) {
    let state = app.state::<AppState>();
    if state.is_renderer_ready() {
        if let Some(main) = app.get_webview_window("main") {
            let _ = main.emit(CLIP_LINK_EVENT, raw.trim());
            return;
        }
    }
    state.queue_clip_link(raw.trim().to_string());
    // With no workspace window at all there is nobody to write it.
    if app.get_webview_window("main").is_none() {
        crate::tray::reveal_tray_hidden_windows(app);
    }
}

/// What the clipboard check found.
#[derive(serde::Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ClipboardCheck {
    /// The text the link vouched for (CRLF turned back into LF when that is
    /// what matched).
    Verified { text: String },
    /// The clipboard is empty, unreadable, or holds something else.
    NotArrived,
    /// Larger than a clip may be.
    TooLarge,
}

fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Port of `verifiedClipboardText`, with upstream's size check before it.
pub fn check_clipboard_text(text: Option<String>, sha256: &str) -> ClipboardCheck {
    let Some(text) = text.filter(|t| !t.is_empty()) else { return ClipboardCheck::NotArrived };
    if text.len() > MAX_CLIP_NOTE_BYTES {
        return ClipboardCheck::TooLarge;
    }
    if sha256_hex(&text) == sha256 {
        return ClipboardCheck::Verified { text };
    }
    if !text.contains("\r\n") {
        return ClipboardCheck::NotArrived;
    }
    let lf = text.replace("\r\n", "\n");
    if sha256_hex(&lf) == sha256 {
        ClipboardCheck::Verified { text: lf }
    } else {
        ClipboardCheck::NotArrived
    }
}

/// A clip that is not silent raises the window it landed in (upstream's
/// `focus`), taking it back from the tray if it was hidden there.
#[tauri::command]
pub fn clip_raise_window(app: AppHandle, window: tauri::WebviewWindow) {
    crate::tray::forget_hidden(&app, window.label());
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// The clipboard's text, but only if it is what a clip link vouched for.
#[tauri::command]
pub fn clip_verified_clipboard_text(app: AppHandle, sha256: String) -> ClipboardCheck {
    check_clipboard_text(app.clipboard().read_text().ok(), &sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_only_new_and_daily_under_the_ports_scheme() {
        assert!(is_clip_link("zennotes-rs://new?file=a&content=b"));
        assert!(is_clip_link("ZENNOTES-RS://Daily"));
        assert!(is_clip_link("zennotes-rs:new"));
        assert!(is_clip_link("zennotes-rs:///daily/?content=x"));
        assert!(!is_clip_link("zennotes-rs://open?path=a.md"));
        assert!(!is_clip_link("zennotes-rs://newer"));
        assert!(!is_clip_link("zennotes://new"));
        assert!(!is_clip_link("/home/me/new.md"));
    }

    #[test]
    fn files_the_clipboard_only_when_it_matches() {
        let text = "# Article\n\nBody".to_string();
        let hash = sha256_hex(&text);
        assert_eq!(check_clipboard_text(Some(text.clone()), &hash), ClipboardCheck::Verified { text: text.clone() });
        assert_eq!(check_clipboard_text(Some("password123".into()), &hash), ClipboardCheck::NotArrived);
        assert_eq!(check_clipboard_text(None, &hash), ClipboardCheck::NotArrived);
        assert_eq!(check_clipboard_text(Some(String::new()), &sha256_hex("")), ClipboardCheck::NotArrived);
        // A Windows browser may store CRLF; the LF text is what gets filed.
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(check_clipboard_text(Some(crlf), &hash), ClipboardCheck::Verified { text });
        let huge = "x".repeat(MAX_CLIP_NOTE_BYTES + 1);
        assert_eq!(check_clipboard_text(Some(huge.clone()), &sha256_hex(&huge)), ClipboardCheck::TooLarge);
    }
}
