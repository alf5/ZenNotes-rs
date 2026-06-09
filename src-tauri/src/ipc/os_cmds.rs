//! OS integration commands (M12): reveal, zoom, fonts, icon.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};

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
