//! OS integration commands (M12): reveal, zoom, fonts, icon.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};

use crate::ipc::types::OpenExternalFileResult;
use crate::os;
use crate::state::AppState;
use crate::vault::config;

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))
}

fn set_zoom(app: &AppHandle, factor: f64) -> Result<f64, String> {
    let clamped = factor.clamp(0.5, 3.0);
    let rounded = (clamped * 100.0).round() / 100.0;
    os::apply_zoom(app, rounded);
    let dir = config_dir(app)?;
    config::update_config(&dir, move |cfg| cfg.zoom_factor = rounded)
        .map_err(|e| format!("persist failed: {e}"))?;
    Ok(rounded)
}

#[tauri::command]
pub fn app_zoom_in(app: AppHandle) -> Result<f64, String> {
    let current = config::load_config(&config_dir(&app)?).zoom_factor;
    set_zoom(&app, current + 0.1)
}

#[tauri::command]
pub fn app_zoom_out(app: AppHandle) -> Result<f64, String> {
    let current = config::load_config(&config_dir(&app)?).zoom_factor;
    set_zoom(&app, current - 0.1)
}

#[tauri::command]
pub fn app_zoom_reset(app: AppHandle) -> Result<f64, String> {
    set_zoom(&app, 1.0)
}

#[tauri::command]
pub fn app_list_fonts() -> Vec<String> {
    os::list_system_fonts()
}

#[tauri::command]
pub fn app_icon_data_url() -> Option<String> {
    Some(os::icon_data_url())
}

#[tauri::command]
pub fn vault_reveal_note(app: AppHandle, state: State<AppState>, rel_path: String) -> Result<(), String> {
    os::reveal_note(&app, &state, &rel_path, false)
}

#[tauri::command]
pub fn vault_reveal_note_target(app: AppHandle, state: State<AppState>, rel_path: String) -> Result<(), String> {
    os::reveal_note(&app, &state, &rel_path, true)
}

#[tauri::command]
pub fn vault_reveal_folder(app: AppHandle, state: State<AppState>, folder: String, subpath: String) -> Result<(), String> {
    os::reveal_folder(&app, &state, &folder, &subpath, false)
}

#[tauri::command]
pub fn vault_reveal_folder_target(app: AppHandle, state: State<AppState>, folder: String, subpath: String) -> Result<(), String> {
    os::reveal_folder(&app, &state, &folder, &subpath, true)
}

#[tauri::command]
pub fn vault_reveal_assets_dir(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    os::reveal_assets_dir(&app, &state)
}

// ---- v2.15 phase A --------------------------------------------------------

#[tauri::command]
pub fn vault_reveal_file_path(app: AppHandle, abs_path: String) -> Result<(), String> {
    os::reveal_file_path(&app, &abs_path)
}

/// `vault:open-external-file` — never throws; the `{ok, error}` shape drives
/// the renderer's toast handling.
#[tauri::command]
pub fn vault_open_external_file(app: AppHandle, href: String) -> OpenExternalFileResult {
    match os::open_external_file(&app, &href) {
        Ok(()) => OpenExternalFileResult { ok: true, error: None },
        Err(error) => OpenExternalFileResult { ok: false, error: Some(error) },
    }
}

/// `vault:fetch-link-metadata` — open-graph fetch for bookmark cards. Runs
/// the blocking HTTP client off the async runtime; never errors (the
/// `{ok:false}` shape drives the renderer's bare-card fallback).
#[tauri::command]
pub async fn vault_fetch_link_metadata(url: String) -> crate::ipc::types::LinkMetadata {
    let for_fail = url.clone();
    tauri::async_runtime::spawn_blocking(move || crate::link_metadata::fetch_link_metadata(&url))
        .await
        .unwrap_or_else(|_| crate::ipc::types::LinkMetadata::fail(for_fail))
}

/// One request to api.unsplash.com for the cover picker (`unsplash.rs`). An
/// `Err` means Unsplash could not be reached; any HTTP answer is `Ok`.
#[tauri::command]
pub async fn unsplash_request(
    url: String,
    key: Option<String>,
) -> Result<crate::unsplash::UnsplashReply, String> {
    tauri::async_runtime::spawn_blocking(move || crate::unsplash::request(&url, key.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// Whether an Access Key is kept. The key itself never crosses the bridge.
#[tauri::command]
pub async fn unsplash_key_is_set() -> bool {
    tauri::async_runtime::spawn_blocking(crate::unsplash::key_is_set)
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn unsplash_key_can_store() -> bool {
    tauri::async_runtime::spawn_blocking(crate::unsplash::key_can_store)
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn unsplash_key_set(key: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || crate::unsplash::key_set(&key))
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn unsplash_key_clear() {
    let _ = tauri::async_runtime::spawn_blocking(crate::unsplash::key_clear).await;
}

/// `devtools:toggle` — Settings → Developer tools. Release builds have the
/// inspector too (Cargo feature `devtools`), as upstream's do.
#[tauri::command]
pub fn devtools_toggle(window: tauri::WebviewWindow) {
    if window.is_devtools_open() {
        window.close_devtools();
    } else {
        window.open_devtools();
    }
}

/// Electron's `clipboard.writeText`: works without focus or a user gesture,
/// which `navigator.clipboard` does not promise.
#[tauri::command]
pub fn app_clipboard_write_text(app: AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

/// Which of these dropped paths are folders (they open as a temporary session,
/// where a file is imported or opened).
#[tauri::command]
pub fn app_dropped_folders(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().filter(|path| std::path::Path::new(path).is_dir()).collect()
}

fn staged_drops_dir() -> PathBuf {
    std::env::temp_dir().join("zennotes-rs-drops")
}

/// Best effort at launch: what a past session staged has long been imported.
pub fn clear_staged_drops() {
    let _ = std::fs::remove_dir_all(staged_drops_dir());
}

/// The last path component only, so a dropped name cannot climb out of its folder.
fn staged_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        "dropped-file".to_string()
    } else {
        base.to_string()
    }
}

fn stage_bytes(dir: &std::path::Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let folder = dir.join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&folder).map_err(|e| format!("Could not stage the dropped file: {e}"))?;
    let path = folder.join(staged_file_name(name));
    std::fs::write(&path, bytes).map_err(|e| format!("Could not stage the dropped file: {e}"))?;
    Ok(path)
}

/// macOS and Windows hand a drop to the page as a `File` with no path, and the
/// importer works on paths. The bytes land in a temp file it can copy from.
/// Raw body, with the percent-encoded name in the `x-file-name` header.
#[tauri::command]
pub fn app_stage_dropped_file(request: tauri::ipc::Request<'_>) -> Result<String, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Could not read the dropped file.".to_string());
    };
    let name = request
        .headers()
        .get("x-file-name")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| urlencoding::decode(value).ok())
        .map(|value| value.into_owned())
        .unwrap_or_default();
    Ok(stage_bytes(&staged_drops_dir(), &name, bytes)?.to_string_lossy().to_string())
}

/// A link straight at a vault asset opens with its default app (upstream's
/// `will-navigate`), and only inside the open vault.
#[tauri::command]
pub fn app_open_vault_asset_url(app: AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let abs = crate::asset_protocol::vault_path_from_url(&app, &url)
        .ok_or_else(|| "This file is outside the open vault.".to_string())?;
    app.opener().open_path(abs, None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod staged_drop_tests {
    use super::*;

    #[test]
    fn a_dropped_name_stays_in_its_folder() {
        assert_eq!(staged_file_name("photo 1.png"), "photo 1.png");
        assert_eq!(staged_file_name("../../etc/passwd"), "passwd");
        assert_eq!(staged_file_name("..\\..\\evil.exe"), "evil.exe");
        assert_eq!(staged_file_name(".."), "dropped-file");
        assert_eq!(staged_file_name(""), "dropped-file");
    }

    #[test]
    fn stages_bytes_under_a_folder_of_its_own() {
        let dir = std::env::temp_dir().join(format!("zn-stage-{}", uuid::Uuid::new_v4().simple()));
        let a = stage_bytes(&dir, "a.png", b"one").unwrap();
        let b = stage_bytes(&dir, "a.png", b"two").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(a.file_name().unwrap(), "a.png");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
