//! Window management, quick-capture, global shortcut, and external-file
//! editing — Tauri-native ports of the corresponding pieces of
//! apps/desktop/src/main/{index.ts,window-vaults.ts}.
//!
//! Note: the single-vault state model means every window shares the active
//! vault (per-window distinct vaults from the Electron `WindowVaultRegistry`
//! are not reproduced). Floating note, quick-capture and external-file windows
//! all work against the active vault.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WebviewWindow};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::ipc::types::{ExternalFileContent, MoveExternalFileResult, VaultInfo};
use crate::state::AppState;
use crate::vault::config::resolve_path;
use crate::vault::notes::to_posix;

const QUICK_CAPTURE_LABEL: &str = "quick-capture";

fn unique_label(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// `window:open-note` — open a note in a detached floating window.
pub fn open_note_window(app: &AppHandle, rel_path: &str) -> Result<(), String> {
    let enc = urlencoding::encode(rel_path);
    let url = format!("index.html?floating=1&note={enc}");
    WebviewWindowBuilder::new(app, unique_label("floating"), WebviewUrl::App(url.into()))
        .title(rel_path.rsplit('/').next().unwrap_or("Note"))
        .inner_size(620.0, 720.0)
        .build()
        .map_err(|e| format!("Failed to open note window: {e}"))?;
    Ok(())
}

/// `window:open-vault` — open another workspace window on the active vault.
pub fn open_vault_window(app: &AppHandle, state: &AppState) -> Result<Option<VaultInfo>, String> {
    WebviewWindowBuilder::new(app, unique_label("workspace"), WebviewUrl::App("index.html".into()))
        .title("ZenNotes-rs")
        .inner_size(1200.0, 800.0)
        .build()
        .map_err(|e| format!("Failed to open vault window: {e}"))?;
    Ok(state.current())
}

fn quick_capture_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(QUICK_CAPTURE_LABEL)
}

/// `window:toggle-quick-capture` — show/hide the quick-capture panel.
pub fn toggle_quick_capture(app: &AppHandle, pinned: bool) -> Result<(), String> {
    if let Some(win) = quick_capture_window(app) {
        let visible = win.is_visible().unwrap_or(false);
        if visible {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        QUICK_CAPTURE_LABEL,
        WebviewUrl::App("index.html?quickCapture=1".into()),
    )
    .title("Quick Capture")
    .inner_size(640.0, 420.0)
    .always_on_top(pinned)
    .skip_taskbar(true)
    .build()
    .map_err(|e| format!("Failed to open quick capture: {e}"))?;
    Ok(())
}

pub fn set_quick_capture_pinned(app: &AppHandle, pinned: bool) {
    if let Some(win) = quick_capture_window(app) {
        let _ = win.set_always_on_top(pinned);
    }
}

/// Register (or re-register) the global quick-capture shortcut. An empty
/// string disables it. Returns the normalized hotkey on success.
pub fn register_quick_capture_shortcut(app: &AppHandle, hotkey: &str) -> Result<String, String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let trimmed = hotkey.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let shortcut: tauri_plugin_global_shortcut::Shortcut = trimmed
        .parse()
        .map_err(|_| format!("Invalid shortcut: {trimmed}"))?;
    gs.on_shortcut(shortcut, move |app, _sc, event| {
        if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
            let pinned = read_pinned(app);
            let _ = toggle_quick_capture(app, pinned);
        }
    })
    .map_err(|e| format!("Failed to register shortcut: {e}"))?;
    Ok(trimmed.to_string())
}

/// Read the persisted quick-capture pinned flag (best-effort).
pub fn read_pinned(app: &AppHandle) -> bool {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| crate::vault::config::load_config(&dir).quick_capture_pinned)
        .unwrap_or(false)
}

// ---- external files ------------------------------------------------------

/// `app:open-markdown-file` — open an absolute `.md` path. Inside the active
/// vault → open as a note in the main window; otherwise a standalone window.
pub fn open_markdown_file(app: &AppHandle, state: &AppState, abs_path: &str) -> Result<bool, String> {
    let abs = resolve_path(abs_path);
    if !abs.to_lowercase().ends_with(".md") {
        return Ok(false);
    }
    if !Path::new(&abs).is_file() {
        return Ok(false);
    }
    if let Some(root) = state.current_root() {
        let root_abs = resolve_path(&root.to_string_lossy());
        let sep = std::path::MAIN_SEPARATOR;
        if let Some(rel) = abs.strip_prefix(&format!("{root_abs}{sep}")) {
            let rel = to_posix(rel);
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.emit("app://open-note", rel);
                let _ = main.set_focus();
                return Ok(true);
            }
        }
    }
    // Outside any vault → standalone external editor window.
    let label = unique_label("external");
    state.set_external_file(&label, PathBuf::from(&abs));
    let name = Path::new(&abs).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html?externalFile=1".into()))
        .title(&name)
        .inner_size(820.0, 720.0)
        .build()
        .map_err(|e| format!("Failed to open file window: {e}"))?;
    Ok(true)
}

pub fn read_external_file(window: &WebviewWindow, state: &AppState) -> Result<ExternalFileContent, String> {
    let path = state
        .external_file(window.label())
        .ok_or_else(|| "No external file bound to this window".to_string())?;
    let body = std::fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
    Ok(ExternalFileContent {
        name: path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        path: path.to_string_lossy().to_string(),
        body,
    })
}

pub fn write_external_file(window: &WebviewWindow, state: &AppState, body: &str) -> Result<(), String> {
    let path = state
        .external_file(window.label())
        .ok_or_else(|| "No external file bound to this window".to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("write failed: {e}"))
}

/// Move the window's external file into the active vault as a note.
pub fn move_external_file_to_vault(
    window: &WebviewWindow,
    state: &AppState,
) -> Result<MoveExternalFileResult, String> {
    let path = state
        .external_file(window.label())
        .ok_or_else(|| "No external file bound to this window".to_string())?;
    let root = state.current_root().ok_or_else(|| "No vault is open".to_string())?;
    let body = std::fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
    let base = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled".to_string());
    // Write into the primary notes folder via the note-create path.
    let meta = crate::vault::crud::create_note(&root, "inbox", Some(&base), "")?;
    crate::vault::crud::write_note(&root, &meta.path, &body)?;
    let _ = std::fs::remove_file(&path);
    state.set_external_file(window.label(), root.join(meta.path.replace('/', std::path::MAIN_SEPARATOR_STR)));
    Ok(MoveExternalFileResult {
        vault_root: root.to_string_lossy().to_string(),
        rel_path: meta.path,
    })
}
