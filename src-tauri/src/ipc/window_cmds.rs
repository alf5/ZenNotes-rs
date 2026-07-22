//! Window / quick-capture / external-file commands (M11).

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::ipc::types::{ExternalFileContent, MoveExternalFileResult, SetHotkeyResult, VaultInfo};
use crate::state::AppState;
use crate::vault::config;
use crate::windows;

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))
}

#[tauri::command]
pub fn window_open_note(app: AppHandle, rel_path: String) -> Result<(), String> {
    windows::open_note_window(&app, &rel_path)
}

#[tauri::command]
pub fn window_open_vault(
    app: AppHandle,
    state: State<AppState>,
    root: Option<String>,
) -> Result<Option<VaultInfo>, String> {
    // v2.15 contract: an explicit root opens that vault directly (no picker).
    // This backend keeps a single active vault shared by all windows, so
    // switch first, then open the new window — existing windows follow.
    // Per-window vault sessions are a later milestone (see GAP-ANALYSIS.md).
    if let Some(root) = root.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        crate::ipc::vault_cmds::open_local_vault_root(&app, &state, root)?;
    }
    windows::open_vault_window(&app, &state)
}

#[tauri::command]
pub fn window_toggle_quick_capture(app: AppHandle) -> Result<(), String> {
    let pinned = windows::read_pinned(&app);
    windows::toggle_quick_capture(&app, pinned)
}

#[tauri::command]
pub fn app_get_quick_capture_hotkey(app: AppHandle) -> Result<String, String> {
    Ok(config::load_config(&config_dir(&app)?).quick_capture_hotkey)
}

#[tauri::command]
pub fn app_set_quick_capture_hotkey(app: AppHandle, hotkey: String) -> Result<SetHotkeyResult, String> {
    match windows::register_quick_capture_shortcut(&app, &hotkey) {
        Ok(normalized) => {
            let dir = config_dir(&app)?;
            let h = normalized.clone();
            config::update_config(&dir, move |cfg| cfg.quick_capture_hotkey = h)
                .map_err(|e| format!("persist failed: {e}"))?;
            Ok(SetHotkeyResult { ok: true, hotkey: normalized, error: None })
        }
        Err(err) => Ok(SetHotkeyResult { ok: false, hotkey: String::new(), error: Some(err) }),
    }
}

#[tauri::command]
pub fn app_get_quick_capture_pinned(app: AppHandle) -> Result<bool, String> {
    Ok(config::load_config(&config_dir(&app)?).quick_capture_pinned)
}

#[tauri::command]
pub fn app_set_quick_capture_pinned(app: AppHandle, pinned: bool) -> Result<bool, String> {
    let dir = config_dir(&app)?;
    config::update_config(&dir, move |cfg| cfg.quick_capture_pinned = pinned)
        .map_err(|e| format!("persist failed: {e}"))?;
    windows::set_quick_capture_pinned(&app, pinned);
    Ok(pinned)
}

#[tauri::command]
pub fn app_open_markdown_file(app: AppHandle, state: State<AppState>, abs_path: String) -> Result<bool, String> {
    windows::open_markdown_file(&app, &state, &abs_path)
}

#[tauri::command]
pub fn app_read_external_file(window: WebviewWindow, state: State<AppState>) -> Result<ExternalFileContent, String> {
    windows::read_external_file(&window, &state)
}

#[tauri::command]
pub fn app_write_external_file(window: WebviewWindow, state: State<AppState>, body: String) -> Result<(), String> {
    windows::write_external_file(&window, &state, &body)
}

#[tauri::command]
pub fn app_move_external_file_to_vault(
    window: WebviewWindow,
    state: State<AppState>,
) -> Result<MoveExternalFileResult, String> {
    windows::move_external_file_to_vault(&window, &state)
}
