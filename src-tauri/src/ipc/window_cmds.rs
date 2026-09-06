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

// ---- v2.45 ----------------------------------------------------------------

/// `openFileDialog` — a native "Open File…" for a markdown file, opened the
/// same vault-aware way as `openMarkdownFile` (upstream #449): a file inside
/// the open vault becomes a note, anything else gets a standalone window.
#[tauri::command]
pub async fn app_open_file_dialog(app: AppHandle, state: State<'_, AppState>) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Open markdown file")
        .add_filter("Markdown", &["md", "markdown"])
        .blocking_pick_file();
    let Some(file_path) = picked else {
        return Ok(false);
    };
    let path = file_path
        .into_path()
        .map_err(|e| format!("Invalid file selection: {e}"))?;
    windows::open_markdown_file(&app, &state, &path.to_string_lossy())
}

/// `followExternalFileLink` — resolve a link clicked in a standalone
/// external-file window against that file's own directory, then open the
/// result: a markdown target the vault-aware way, anything else with the OS.
/// Never throws; the `{ok, error}` shape drives the renderer's toast.
#[tauri::command]
pub fn external_file_follow_link(
    app: AppHandle,
    window: WebviewWindow,
    state: State<AppState>,
    link: crate::standalone_links::ExternalFileLink,
) -> crate::ipc::types::OpenExternalFileResult {
    use crate::standalone_links::{resolve_standalone_link, LinkTarget};

    let result = (|| -> Result<(), String> {
        let note_path = state
            .external_file(window.label())
            .ok_or_else(|| "This window has no file behind it.".to_string())?;
        match resolve_standalone_link(&note_path, &link) {
            Some(LinkTarget::Markdown(abs)) => {
                windows::open_markdown_file(&app, &state, &abs.to_string_lossy()).map(|_| ())
            }
            Some(LinkTarget::File(abs)) => {
                crate::os::open_external_file(&app, &abs.to_string_lossy())
            }
            None => Err("That link does not point at a file next to this one.".into()),
        }
    })();
    match result {
        Ok(()) => crate::ipc::types::OpenExternalFileResult { ok: true, error: None },
        Err(error) => {
            crate::ipc::types::OpenExternalFileResult { ok: false, error: Some(error) }
        }
    }
}
