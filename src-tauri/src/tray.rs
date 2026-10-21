//! "Keep running in the tray" (#947) — port of apps/desktop/src/main/tray.ts,
//! last-window.ts and the close / hide wiring in index.ts.
//!
//! With it on, closing the last visible workspace window hides it instead of
//! ending the session, so the Quick capture hotkey keeps working — which on a
//! tiling window manager, where there is nowhere to minimise to, is the whole
//! point. The icon is how a hidden app stays reachable and honest: a click
//! brings the windows back, the menu quits for real, and a connected vault's
//! sync state shows in the tooltip and on a menu line.
//!
//! On Linux the icon goes through libayatana-appindicator (or the older
//! libappindicator), which tray-icon loads at runtime and PANICS on when
//! neither is installed; with `panic = "abort"` that would take the app down.
//! So the libraries are probed first, and without one the setting simply
//! stays off (the setter resolves to the value in effect, as upstream's does).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WebviewWindow, Window, WindowEvent};

use crate::state::AppState;
use crate::vault::config;

const TRAY_ID: &str = "zennotes-tray";
const QUICK_CAPTURE_LABEL: &str = "quick-capture";
const PRODUCT: &str = "ZenNotes-rs";
const MAX_VAULT_NAME_LENGTH: usize = 120;

/// What a window reports about its Cloud sync (`TraySyncStatus`). This build
/// has no Cloud sync, so app-core reports null and the line never shows; it
/// is kept so the contract member does what it says if that ever changes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraySyncStatus {
    pub state: String,
    pub vault_name: Option<String>,
    pub last_synced_at: Option<f64>,
}

#[derive(Default)]
pub struct TrayState {
    keep_running: AtomicBool,
    /// Set once the app is really exiting, so close-to-tray stands aside.
    quitting: AtomicBool,
    /// Whether a window has closed in this run (see `should_quit_with_only_quick_capture_left`).
    window_closed: AtomicBool,
    /// Workspace windows a close hid to the tray, so the tray brings back
    /// exactly those and never a window still loading behind `visible: false`.
    hidden: Mutex<HashSet<String>>,
    icon: Mutex<Option<TrayIcon>>,
    sync: Mutex<Option<TraySyncStatus>>,
}

/// Port of `parseTraySyncStatus`: the renderer is untrusted, keep only a
/// well-formed status.
pub fn parse_tray_sync_status(raw: &Value) -> Option<TraySyncStatus> {
    let obj = raw.as_object()?;
    let state = obj.get("state")?.as_str()?;
    if !["idle", "syncing", "attention", "error"].contains(&state) {
        return None;
    }
    let vault_name = obj
        .get("vaultName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(MAX_VAULT_NAME_LENGTH).collect());
    let last_synced_at = obj
        .get("lastSyncedAt")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0);
    Some(TraySyncStatus { state: state.to_string(), vault_name, last_synced_at })
}

/// Port of `traySyncLine`. A menu is not redrawn while it is open, so a time
/// is a clock time ("synced at 14:05"), which stays true.
pub fn tray_sync_line(status: Option<&TraySyncStatus>, format_time: impl Fn(f64) -> String) -> Option<String> {
    let status = status?;
    Some(match status.state.as_str() {
        "syncing" => "Cloud: syncing…".to_string(),
        "attention" => "Cloud: needs attention".to_string(),
        "error" => "Cloud: sync failed".to_string(),
        _ => match status.last_synced_at {
            None => "Cloud: ready".to_string(),
            Some(at) => format!("Cloud: synced at {}", format_time(at)),
        },
    })
}

pub fn tray_tooltip(sync_line: Option<&str>) -> String {
    match sync_line {
        Some(line) => format!("{PRODUCT}\n{line}"),
        None => PRODUCT.to_string(),
    }
}

/// Port of `shouldHideOnClose`: only the last visible workspace window hides;
/// closing one of several still closes it, and quitting always lets windows
/// close, or the app could never exit.
pub fn should_hide_on_close(keep_running: bool, quitting: bool, is_workspace: bool, other_visible_workspace: usize) -> bool {
    keep_running && !quitting && is_workspace && other_visible_workspace == 0
}

/// Port of last-window.ts `shouldQuitWithOnlyQuickCaptureLeft`. The Quick
/// capture panel hides instead of closing, and a hidden window still keeps
/// the app alive: once the panel had been used, closing the last window left
/// the app running with nothing on screen and no icon. A hidden panel is not
/// a window anyone can get back to, so it does not keep the app alive.
pub fn should_quit_with_only_quick_capture_left(
    is_mac: bool,
    keep_running: bool,
    quitting: bool,
    window_closed: bool,
    other_windows: usize,
    quick_capture: QuickCapture,
) -> bool {
    !is_mac && !keep_running && !quitting && window_closed && other_windows == 0 && quick_capture == QuickCapture::Hidden
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickCapture {
    None,
    Hidden,
    Visible,
}

pub fn is_workspace_window(label: &str) -> bool {
    label == "main" || label.starts_with("workspace-")
}

/// Whether a tray icon can be shown at all on this desktop.
#[cfg(target_os = "linux")]
fn tray_supported() -> bool {
    // The names libappindicator-sys tries, in its order.
    ["libayatana-appindicator3.so.1", "libappindicator3.so.1", "libayatana-appindicator3.so", "libappindicator3.so"]
        .iter()
        .any(|name| unsafe { libloading::Library::new(name) }.is_ok())
}

#[cfg(not(target_os = "linux"))]
fn tray_supported() -> bool {
    true
}

fn tray_state(app: &AppHandle) -> State<'_, TrayState> {
    app.state::<TrayState>()
}

fn format_clock_time(epoch_ms: f64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

fn build_menu(app: &AppHandle, sync_line: Option<&str>) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "tray-open", format!("Open {PRODUCT}"), true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "tray-capture", "Quick Capture", true, None::<&str>)?)?;
    if let Some(line) = sync_line {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&MenuItem::with_id(app, "tray-sync", line, false, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "tray-quit", format!("Quit {PRODUCT}"), true, None::<&str>)?)?;
    Ok(menu)
}

fn refresh_tray(app: &AppHandle) {
    let state = tray_state(app);
    let sync_line = tray_sync_line(state.sync.lock().unwrap().as_ref(), format_clock_time);
    let icon = state.icon.lock().unwrap().clone();
    if let Some(icon) = icon {
        let _ = icon.set_tooltip(Some(tray_tooltip(sync_line.as_deref())));
        if let Ok(menu) = build_menu(app, sync_line.as_deref()) {
            let _ = icon.set_menu(Some(menu));
        }
    }
}

/// Create the icon if it is not there yet.
fn show_tray(app: &AppHandle) -> Result<(), String> {
    let state = tray_state(app);
    if state.icon.lock().unwrap().is_some() {
        refresh_tray(app);
        return Ok(());
    }
    if !tray_supported() {
        return Err("No system tray library (libayatana-appindicator3) is installed".into());
    }
    let menu = build_menu(app, None).map_err(|e| e.to_string())?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tray_tooltip(None))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-open" => reveal_tray_hidden_windows(app),
            "tray-capture" => {
                let pinned = crate::windows::read_pinned(app);
                let _ = crate::windows::toggle_quick_capture(app, pinned);
            }
            "tray-quit" => quit(app),
            _ => {}
        })
        // A left click opens the window. Most Linux hosts open the menu on any
        // click instead, and the menu's first item does the same.
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                reveal_tray_hidden_windows(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder.build(app).map_err(|e| e.to_string())?;
    *state.icon.lock().unwrap() = Some(icon);
    refresh_tray(app);
    Ok(())
}

fn hide_tray(app: &AppHandle) {
    if let Some(icon) = tray_state(app).icon.lock().unwrap().take() {
        let _ = icon.set_visible(false);
    }
}

/// Bring back every window the tray is holding, the main one in front. With
/// none held and none open, open a workspace window.
pub fn reveal_tray_hidden_windows(app: &AppHandle) {
    let held: Vec<String> = tray_state(app).hidden.lock().unwrap().drain().collect();
    for label in &held {
        if label != "main" {
            if let Some(win) = app.get_webview_window(label) {
                let _ = win.show();
            }
        }
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
        return;
    }
    if let Some(win) = held.last().and_then(|label| app.get_webview_window(label)) {
        let _ = win.set_focus();
        return;
    }
    if !app.webview_windows().keys().any(|label| is_workspace_window(label)) {
        let state = app.state::<AppState>();
        let _ = crate::windows::open_vault_window(app, &state);
    }
}

fn has_visible_workspace_window(app: &AppHandle) -> bool {
    app.webview_windows()
        .iter()
        .any(|(label, win)| is_workspace_window(label) && win.is_visible().unwrap_or(false))
}

/// Turn the tray on or off; returns the setting now in effect.
pub fn apply_keep_running_in_tray(app: &AppHandle, enabled: bool) -> bool {
    let state = tray_state(app);
    if !enabled {
        state.keep_running.store(false, Ordering::SeqCst);
        hide_tray(app);
        // Nothing would bring a held window back once the icon is gone.
        if !state.hidden.lock().unwrap().is_empty() {
            reveal_tray_hidden_windows(app);
        }
        return false;
    }
    match show_tray(app) {
        Ok(()) => {
            state.keep_running.store(true, Ordering::SeqCst);
            true
        }
        Err(error) => {
            eprintln!("tray: {error}");
            state.keep_running.store(false, Ordering::SeqCst);
            false
        }
    }
}

/// A real hide, not a minimise: a tiling window manager has nowhere to
/// minimise to, which is the case the tray exists for.
fn hide_window_to_tray(window: &Window) {
    tray_state(window.app_handle()).hidden.lock().unwrap().insert(window.label().to_string());
    let _ = window.hide();
}

fn quit(app: &AppHandle) {
    tray_state(app).quitting.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// Checked after any window closes and when the Quick capture panel hides;
/// deferred a beat so the closing window is gone before windows are counted.
pub fn schedule_quit_if_only_quick_capture_left(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let state = tray_state(&app);
        let windows = app.webview_windows();
        let capture = windows.get(QUICK_CAPTURE_LABEL);
        let quick_capture = match capture {
            None => QuickCapture::None,
            Some(win) if win.is_visible().unwrap_or(false) => QuickCapture::Visible,
            Some(_) => QuickCapture::Hidden,
        };
        let others = windows.keys().filter(|label| label.as_str() != QUICK_CAPTURE_LABEL).count();
        if should_quit_with_only_quick_capture_left(
            cfg!(target_os = "macos"),
            state.keep_running.load(Ordering::SeqCst),
            state.quitting.load(Ordering::SeqCst),
            state.window_closed.load(Ordering::SeqCst),
            others,
            quick_capture,
        ) {
            quit(&app);
        }
    });
}

/// Window events the tray cares about (wired in lib.rs).
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let app = window.app_handle();
    let state = tray_state(app);
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            let label = window.label();
            let others = app
                .webview_windows()
                .iter()
                .filter(|(other, win)| {
                    other.as_str() != label && is_workspace_window(other) && win.is_visible().unwrap_or(false)
                })
                .count();
            if should_hide_on_close(
                state.keep_running.load(Ordering::SeqCst),
                state.quitting.load(Ordering::SeqCst),
                is_workspace_window(label),
                others,
            ) {
                api.prevent_close();
                hide_window_to_tray(window);
            }
        }
        WindowEvent::Destroyed => {
            state.hidden.lock().unwrap().remove(window.label());
            state.window_closed.store(true, Ordering::SeqCst);
            schedule_quit_if_only_quick_capture_left(app);
        }
        _ => {}
    }
}

/// Opening the app again (a second launch, a dock click) must show a window,
/// including the ones the tray holds.
pub fn on_reopen(app: &AppHandle) {
    if !has_visible_workspace_window(app) || !tray_state(app).hidden.lock().unwrap().is_empty() {
        reveal_tray_hidden_windows(app);
    } else if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_focus();
    }
}

/// At launch: put the icon back when the setting was left on.
pub fn restore_at_launch(app: &AppHandle) {
    if let Ok(dir) = app.path().app_config_dir() {
        if config::load_config(&dir).keep_running_in_tray {
            apply_keep_running_in_tray(app, true);
        }
    }
}

#[tauri::command]
pub fn app_get_keep_running_in_tray(app: AppHandle) -> bool {
    tray_state(&app).keep_running.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn app_set_keep_running_in_tray(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let applied = apply_keep_running_in_tray(&app, enabled);
    // Kept in the machine-local app config, not config.toml: whether a tray
    // exists at all depends on this machine's desktop, and it is needed
    // before any renderer runs.
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    config::update_config(&dir, move |cfg| cfg.keep_running_in_tray = applied).map_err(|e| e.to_string())?;
    Ok(applied)
}

/// Hide this window to the tray. False, leaving the window alone, while the
/// tray is off: a hidden window would be unreachable.
#[tauri::command]
pub fn window_hide_to_tray(window: WebviewWindow) -> bool {
    let app = window.app_handle();
    if !tray_state(app).keep_running.load(Ordering::SeqCst) || !is_workspace_window(window.label()) {
        return false;
    }
    tray_state(app).hidden.lock().unwrap().insert(window.label().to_string());
    let _ = window.hide();
    true
}

#[tauri::command]
pub fn app_quit(app: AppHandle) {
    quit(&app);
}

/// One line for one app: the main window speaks for the tray.
#[tauri::command]
pub fn app_tray_sync_status(app: AppHandle, window: WebviewWindow, status: Value) {
    if window.label() != "main" {
        return;
    }
    *tray_state(&app).sync.lock().unwrap() = parse_tray_sync_status(&status);
    refresh_tray(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_well_formed_sync_status() {
        let ok = parse_tray_sync_status(&serde_json::json!({
            "state": "idle", "vaultName": "  Notes ", "lastSyncedAt": 1_700_000_000_000.0
        }))
        .unwrap();
        assert_eq!(ok.vault_name.as_deref(), Some("Notes"));
        assert!(parse_tray_sync_status(&serde_json::json!({ "state": "bogus" })).is_none());
        assert!(parse_tray_sync_status(&serde_json::json!(null)).is_none());
        let bare = parse_tray_sync_status(&serde_json::json!({ "state": "error", "lastSyncedAt": -1 })).unwrap();
        assert_eq!(bare.last_synced_at, None);
        assert_eq!(bare.vault_name, None);
    }

    #[test]
    fn sync_line_and_tooltip_read_like_upstream() {
        let at = |state: &str, last: Option<f64>| TraySyncStatus { state: state.into(), vault_name: None, last_synced_at: last };
        let fmt = |_: f64| "14:05".to_string();
        assert_eq!(tray_sync_line(None, fmt), None);
        assert_eq!(tray_sync_line(Some(&at("idle", None)), fmt).as_deref(), Some("Cloud: ready"));
        assert_eq!(tray_sync_line(Some(&at("idle", Some(1.0))), fmt).as_deref(), Some("Cloud: synced at 14:05"));
        assert_eq!(tray_sync_line(Some(&at("syncing", None)), fmt).as_deref(), Some("Cloud: syncing…"));
        assert_eq!(tray_tooltip(None), "ZenNotes-rs");
        assert_eq!(tray_tooltip(Some("Cloud: ready")), "ZenNotes-rs\nCloud: ready");
    }

    #[test]
    fn hides_only_the_last_visible_workspace_window() {
        assert!(should_hide_on_close(true, false, true, 0));
        assert!(!should_hide_on_close(true, false, true, 1));
        assert!(!should_hide_on_close(true, true, true, 0));
        assert!(!should_hide_on_close(false, false, true, 0));
        assert!(!should_hide_on_close(true, false, false, 0));
    }

    #[test]
    fn a_hidden_quick_capture_panel_does_not_keep_the_app_alive() {
        use QuickCapture::*;
        assert!(should_quit_with_only_quick_capture_left(false, false, false, true, 0, Hidden));
        assert!(!should_quit_with_only_quick_capture_left(true, false, false, true, 0, Hidden));
        assert!(!should_quit_with_only_quick_capture_left(false, true, false, true, 0, Hidden));
        assert!(!should_quit_with_only_quick_capture_left(false, false, false, false, 0, Hidden));
        assert!(!should_quit_with_only_quick_capture_left(false, false, false, true, 1, Hidden));
        assert!(!should_quit_with_only_quick_capture_left(false, false, false, true, 0, Visible));
        assert!(!should_quit_with_only_quick_capture_left(false, false, false, true, 0, None));
        assert!(is_workspace_window("main") && is_workspace_window("workspace-1"));
        assert!(!is_workspace_window("quick-capture") && !is_workspace_window("floating-1"));
    }
}
