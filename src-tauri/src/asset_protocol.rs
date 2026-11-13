//! `syn-asset://` custom URI scheme — port of the Electron `protocol.handle`
//! for `zen-asset` (apps/desktop/src/main/index.ts). Serves vault asset files
//! to the webview: from the open local vault after checking the path is
//! inside it, or, for a remote workspace, from the server through the session.
//!
//! The frontend builds URLs with `convertFileSrc(path, 'syn-asset')`
//! (src/bridge/asset-url.ts), which percent-encodes the path into a single URI
//! path segment. A local path is absolute; a remote one is the vault-relative
//! path behind `remote:`, which no absolute path on any platform starts with
//! (upstream's `zen-asset://remote?path=`).
//!
//! The handler runs on the blocking pool (lib.rs registers it asynchronously),
//! so neither a large file nor a slow server holds up the main thread.

use std::io::Read;
use std::path::Path;

use tauri::http::{Request, Response};
use tauri::{AppHandle, Manager};

use crate::state::AppState;
use crate::vault::config::resolve_path;

/// Marks a remote vault-relative path in a `syn-asset` URL.
pub const REMOTE_ASSET_PREFIX: &str = "remote:";
/// A remote asset larger than this is not served.
const MAX_REMOTE_ASSET_BYTES: u64 = 1024 * 1024 * 1024;

/// Decode the path from the request URI path component.
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

/// What a `syn-asset` URL points at.
#[derive(Debug, PartialEq)]
pub enum AssetTarget {
    /// A file inside the open local vault (absolute path).
    Local(String),
    /// A vault-relative path on the connected server.
    Remote(String),
}

fn target_for_path(app: &AppHandle, decoded: String) -> Option<AssetTarget> {
    if let Some(rel) = decoded.strip_prefix(REMOTE_ASSET_PREFIX) {
        let rel = rel.trim();
        let connected = app.state::<AppState>().remote_session().is_some();
        return (connected && !rel.is_empty()).then(|| AssetTarget::Remote(rel.to_string()));
    }
    is_inside_vault(app, &decoded).then_some(AssetTarget::Local(decoded))
}

/// The vault file a `syn-asset` URL points at, as the webview sees it on each
/// platform (`syn-asset://localhost/…` or `http://syn-asset.localhost/…`).
pub fn target_from_url(app: &AppHandle, url: &str) -> Option<AssetTarget> {
    let parsed = url::Url::parse(url).ok()?;
    target_for_path(app, decode_asset_path(parsed.path())?)
}

fn status_only(status: u16) -> Response<Vec<u8>> {
    Response::builder().status(status).body(Vec::new()).unwrap()
}

fn remote_response(app: &AppHandle, rel: &str, range: Option<&str>) -> Response<Vec<u8>> {
    let Some(session) = app.state::<AppState>().remote_session() else {
        return status_only(404);
    };
    let response = match session.transport.asset(rel, range) {
        Ok(response) => response,
        Err(err) => return status_only(err.status.unwrap_or(502)),
    };
    let status = response.status();
    let content_type = response
        .header("Content-Type")
        .map(str::to_string)
        .unwrap_or_else(|| mime_type_for_path(rel).to_string());
    let content_range = response.header("Content-Range").map(str::to_string);
    let accept_ranges = response.header("Accept-Ranges").map(str::to_string);
    let mut bytes = Vec::new();
    let read = response.into_reader().take(MAX_REMOTE_ASSET_BYTES + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_REMOTE_ASSET_BYTES {
        return status_only(502);
    }
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Cache-Control", "no-cache")
        .header("Access-Control-Allow-Origin", "*");
    if let Some(range) = content_range {
        builder = builder.header("Content-Range", range);
    }
    if let Some(accept) = accept_ranges {
        builder = builder.header("Accept-Ranges", accept);
    }
    builder.body(bytes).unwrap_or_else(|_| status_only(502))
}

pub fn handle(app: &AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let not_found = || status_only(404);

    let Some(decoded) = decode_asset_path(request.uri().path()) else {
        return not_found();
    };
    let abs = match target_for_path(app, decoded) {
        Some(AssetTarget::Local(abs)) => abs,
        Some(AssetTarget::Remote(rel)) => {
            let range = request.headers().get("Range").and_then(|v| v.to_str().ok());
            return remote_response(app, &rel, range);
        }
        None => return not_found(),
    };
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
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
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
        // ... and a remote path behind its marker.
        let encoded = "/remote%3Aassets%2Fa%20b.png";
        assert_eq!(decode_asset_path(encoded).as_deref(), Some("remote:assets/a b.png"));
    }

    #[test]
    fn mime_lookup() {
        assert_eq!(mime_type_for_path("/v/x.png"), "image/png");
        assert_eq!(mime_type_for_path("/v/x.pdf"), "application/pdf");
        assert_eq!(mime_type_for_path("/v/x.bin"), "application/octet-stream");
    }
}
