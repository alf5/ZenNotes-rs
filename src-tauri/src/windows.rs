//! Window management, quick-capture, global shortcut, and external-file
//! editing — Tauri-native ports of the corresponding pieces of
//! apps/desktop/src/main/{index.ts,window-vaults.ts}.
//!
//! Note: the single-vault state model means every window shares the active
//! vault (per-window distinct vaults from the Electron `WindowVaultRegistry`
//! are not reproduced). Floating note, quick-capture and external-file windows
//! all work against the active vault.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WebviewWindow};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::ipc::types::{ExternalFileContent, MoveExternalFileResult, VaultInfo};
use crate::state::AppState;
use crate::vault::config::resolve_path;
use crate::vault::notes::to_posix;

const QUICK_CAPTURE_LABEL: &str = "quick-capture";

/// Off on macOS and Windows, where the native handler swallows every drag over
/// the webview, in-app ones too (tabs, sidebar, boards). The main window gets
/// the same from tauri.conf.json, and src/bridge/file-drops.ts takes OS drops.
fn with_drop_mode<'a>(
    builder: WebviewWindowBuilder<'a, tauri::Wry, AppHandle>,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    #[cfg(not(target_os = "linux"))]
    let builder = builder.disable_drag_drop_handler();
    builder
}

fn unique_label(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// `window:open-note` — open a note in a detached floating window.
pub fn open_note_window(app: &AppHandle, rel_path: &str) -> Result<(), String> {
    let enc = urlencoding::encode(rel_path);
    let url = format!("index.html?floating=1&note={enc}");
    let builder = WebviewWindowBuilder::new(app, unique_label("floating"), WebviewUrl::App(url.into()))
        .title(rel_path.rsplit('/').next().unwrap_or("Note"))
        .inner_size(620.0, 720.0);
    // Linux ships frameless (like upstream Electron); the app draws its own titlebar.
    #[cfg(target_os = "linux")]
    let builder = builder.decorations(false);
    with_drop_mode(builder)
        .build()
        .map_err(|e| format!("Failed to open note window: {e}"))?;
    Ok(())
}

/// `window:open-vault` — open another workspace window on the active vault.
pub fn open_vault_window(app: &AppHandle, state: &AppState) -> Result<Option<VaultInfo>, String> {
    let builder = WebviewWindowBuilder::new(app, unique_label("workspace"), WebviewUrl::App("index.html".into()))
        .title("ZenNotes-rs")
        .inner_size(1200.0, 800.0);
    #[cfg(target_os = "linux")]
    let builder = builder.decorations(false);
    with_drop_mode(builder)
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
            // A panel left after the last window closed holds the app only
            // until it hides (upstream last-window.ts).
            crate::tray::schedule_quit_if_only_quick_capture_left(app);
        } else {
            // A hidden panel opens on the display under the pointer (#976).
            move_quick_capture_to_pointer_display(app, &win);
            let _ = win.show();
            let _ = win.set_focus();
        }
        return Ok(());
    }
    let builder = WebviewWindowBuilder::new(
        app,
        QUICK_CAPTURE_LABEL,
        WebviewUrl::App("index.html?quickCapture=1".into()),
    )
    .title("Quick Capture")
    .inner_size(640.0, 420.0)
    .always_on_top(pinned)
    .skip_taskbar(true);
    #[cfg(target_os = "linux")]
    let builder = builder.decorations(false);
    with_drop_mode(builder)
        .build()
        .map_err(|e| format!("Failed to open quick capture: {e}"))?;
    Ok(())
}

/// A rectangle on the virtual desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScreenRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl ScreenRect {
    fn contains(&self, r: &ScreenRect) -> bool {
        r.x >= self.x
            && r.y >= self.y
            && r.x + r.width as i32 <= self.x + self.width as i32
            && r.y + r.height as i32 <= self.y + self.height as i32
    }
}

/// Port of upstream `quickCaptureBoundsOnDisplay`: `None` on the display it is
/// already on; back on a display it has been on, where it last sat there if
/// that still fits; otherwise the same relative place in the work area, shrunk
/// to fit and kept inside it.
fn quick_capture_bounds_on_display(
    bounds: ScreenRect,
    from: (&str, ScreenRect),
    to: (&str, ScreenRect),
    remembered: Option<ScreenRect>,
) -> Option<ScreenRect> {
    if from.0 == to.0 {
        return None;
    }
    let (source, area) = (from.1, to.1);
    if let Some(spot) = remembered.filter(|spot| area.contains(spot)) {
        return Some(spot);
    }
    let width = bounds.width.min(area.width);
    let height = bounds.height.min(area.height);
    let fraction = |at: i32, size: u32, start: i32, span: u32| {
        if span == 0 {
            return 0.5;
        }
        ((at as f64 + size as f64 / 2.0 - start as f64) / span as f64).clamp(0.0, 1.0)
    };
    let place = |f: f64, start: i32, span: u32, size: u32| {
        let at = (start as f64 + f * span as f64 - size as f64 / 2.0).round() as i32;
        at.clamp(start, start + (span - size) as i32)
    };
    Some(ScreenRect {
        x: place(fraction(bounds.x, bounds.width, source.x, source.width), area.x, area.width, width),
        y: place(fraction(bounds.y, bounds.height, source.y, source.height), area.y, area.height, height),
        width,
        height,
    })
}

/// Where the panel last sat on each display it left, by monitor name.
static QUICK_CAPTURE_SPOTS: LazyLock<Mutex<HashMap<String, ScreenRect>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn monitor_area(monitor: &tauri::Monitor) -> (String, ScreenRect) {
    let area = monitor.work_area();
    let name = monitor
        .name()
        .cloned()
        .unwrap_or_else(|| format!("{},{}", monitor.position().x, monitor.position().y));
    (
        name,
        ScreenRect {
            x: area.position.x,
            y: area.position.y,
            width: area.size.width,
            height: area.size.height,
        },
    )
}

/// Best effort: a compositor that hides the pointer or refuses to place
/// windows (Wayland) leaves the panel to the window manager, as before.
fn move_quick_capture_to_pointer_display(app: &AppHandle, win: &WebviewWindow) {
    let (Ok(position), Ok(size), Ok(pointer), Ok(Some(current))) = (
        win.outer_position(),
        win.outer_size(),
        app.cursor_position(),
        win.current_monitor(),
    ) else {
        return;
    };
    let Ok(Some(target)) = app.monitor_from_point(pointer.x, pointer.y) else {
        return;
    };
    let bounds = ScreenRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    let (from, to) = (monitor_area(&current), monitor_area(&target));
    let Ok(mut spots) = QUICK_CAPTURE_SPOTS.lock() else {
        return;
    };
    let next = quick_capture_bounds_on_display(
        bounds,
        (&from.0, from.1),
        (&to.0, to.1),
        spots.get(&to.0).copied(),
    );
    if let Some(next) = next {
        spots.insert(from.0, bounds);
        let _ = win.set_size(tauri::PhysicalSize::new(next.width, next.height));
        let _ = win.set_position(tauri::PhysicalPosition::new(next.x, next.y));
    }
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
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html?externalFile=1".into()))
        .title(&name)
        .inner_size(820.0, 720.0);
    with_drop_mode(builder)
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
    let ephemeral = state.current().and_then(|v| v.temporary).unwrap_or(false);
    crate::vault::crud::write_note(&root, &meta.path, &body, ephemeral)?;
    let _ = std::fs::remove_file(&path);
    state.set_external_file(window.label(), root.join(meta.path.replace('/', std::path::MAIN_SEPARATOR_STR)));
    Ok(MoveExternalFileResult {
        vault_root: root.to_string_lossy().to_string(),
        rel_path: meta.path,
    })
}

#[cfg(test)]
mod quick_capture_placement_tests {
    use super::*;

    const LEFT: ScreenRect = ScreenRect { x: 0, y: 0, width: 1920, height: 1080 };
    const RIGHT: ScreenRect = ScreenRect { x: 1920, y: 0, width: 1280, height: 720 };

    fn panel(x: i32, y: i32) -> ScreenRect {
        ScreenRect { x, y, width: 640, height: 420 }
    }

    #[test]
    fn stays_put_on_its_own_display() {
        let next = quick_capture_bounds_on_display(panel(10, 10), ("a", LEFT), ("a", LEFT), None);
        assert_eq!(next, None);
    }

    #[test]
    fn a_centered_panel_opens_centered_on_another_display() {
        let next = quick_capture_bounds_on_display(panel(640, 330), ("a", LEFT), ("b", RIGHT), None);
        assert_eq!(next, Some(panel(1920 + 320, 150)));
    }

    #[test]
    fn returns_to_where_it_last_sat_while_that_still_fits() {
        let spot = panel(2000, 20);
        let next = quick_capture_bounds_on_display(panel(0, 0), ("a", LEFT), ("b", RIGHT), Some(spot));
        assert_eq!(next, Some(spot));
        let gone = panel(9000, 20);
        let next = quick_capture_bounds_on_display(panel(0, 0), ("a", LEFT), ("b", RIGHT), Some(gone));
        assert_ne!(next, Some(gone));
    }

    #[test]
    fn shrinks_to_fit_and_stays_inside_a_smaller_work_area() {
        let small = ScreenRect { x: -500, y: 0, width: 500, height: 300 };
        let next = quick_capture_bounds_on_display(panel(1280, 660), ("a", LEFT), ("b", small), None);
        assert_eq!(next, Some(ScreenRect { x: -500, y: 0, width: 500, height: 300 }));
    }
}
