//! `syn-asset://` custom URI scheme — port of the Electron `protocol.handle`
//! for `zen-asset` (apps/desktop/src/main/index.ts). Serves vault asset files
//! to the webview after validating the path is inside the open local vault.
//!
//! The frontend builds URLs with `convertFileSrc(absPath, 'syn-asset')`
//! (src/bridge/asset-url.ts), which percent-encodes the absolute path into a
//! single URI path segment. We decode it, bounds-check it, and stream bytes.

use std::path::Path;

use tauri::http::{Request, Response};
use tauri::{AppHandle, Manager};

use crate::state::AppState;
use crate::vault::config::resolve_path;

/// Decode the absolute file path from the request URI path component.
fn decode_asset_path(uri_path: &str) -> Option<String> {
    let encoded = uri_path.trim_start_matches('/');
    if encoded.is_empty() {
        return None;
    }
    urlencoding::decode(encoded).ok().map(|s| s.into_owned())
}

fn is_inside_vault(app: &AppHandle, abs: &str) -> bool {
    let Some(root) = app.state::<AppState>().current_root() else {
        return false;
    };
    let root_abs = resolve_path(&root.to_string_lossy());
    let abs_norm = resolve_path(abs);
    let sep = std::path::MAIN_SEPARATOR;
    abs_norm == root_abs || abs_norm.starts_with(&format!("{root_abs}{sep}"))
}

pub fn handle(app: &AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let not_found = || Response::builder().status(404).body(Vec::new()).unwrap();

    let Some(abs) = decode_asset_path(request.uri().path()) else {
        return not_found();
    };
    if !is_inside_vault(app, &abs) {
        return not_found();
    }
    match std::fs::read(&abs) {
        Ok(bytes) => Response::builder()
            .header("Content-Type", mime_type_for_path(&abs))
            .header("Cache-Control", "no-cache")
            .header("Access-Control-Allow-Origin", "*")
            .body(bytes)
            .unwrap(),
        Err(_) => not_found(),
    }
}

/// Port of `mimeTypeForPath`.
pub fn mime_type_for_path(abs_path: &str) -> &'static str {
    let ext = Path::new(abs_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "apng" => "image/apng",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "jpeg" | "jpg" => "image/jpeg",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "m4v" | "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_encoded_abs_path() {
        // convertFileSrc encodes "/Users/a b/pic.png".
        let encoded = "/%2FUsers%2Fa%20b%2Fpic.png";
        assert_eq!(decode_asset_path(encoded).as_deref(), Some("/Users/a b/pic.png"));
    }

    #[test]
    fn mime_lookup() {
        assert_eq!(mime_type_for_path("/v/x.png"), "image/png");
        assert_eq!(mime_type_for_path("/v/x.pdf"), "application/pdf");
        assert_eq!(mime_type_for_path("/v/x.bin"), "application/octet-stream");
    }
}
