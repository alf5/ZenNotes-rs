//! ZenNotes-rs — Tauri (Rust) backend.
//!
//! Reimplements the ZenNotes Electron `main/` process as Tauri commands.
//! The frontend talks to this backend through the same ZenBridge contract
//! it used with Electron; each Electron IPC channel maps to a `#[command]`
//! here (see src/bridge/tauri-bridge.ts on the frontend side).

mod asset_protocol;
mod deep_links;
mod ipc;
mod os;
mod remote;
mod search;
mod secrets;
mod state;
mod vault;
mod watcher;
mod windows;

use ipc::{os_cmds, vault_cmds, window_cmds, workspace_cmds};
use state::AppState;
use tauri::Manager;

/// `app:platform` — the host OS, as Node's `process.platform` strings so
/// the frontend's platform checks keep working unchanged.
#[tauri::command]
fn app_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

/// `app:renderer-ready` — the renderer has mounted and subscribed to
/// open-note events. Flush any deep-link / file-open requests queued during
/// launch to the main window.
#[tauri::command]
fn app_renderer_ready(app: tauri::AppHandle, state: tauri::State<AppState>) {
    use tauri::Emitter;
    state.set_renderer_ready(true);
    if let Some(main) = app.get_webview_window("main") {
        for rel in state.drain_pending_open_notes() {
            let _ = main.emit("app://open-note", rel);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // single-instance must be registered first; it forwards file/deep-link
        // args from a second launch to the running instance.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            for arg in argv.iter().skip(1) {
                deep_links::handle_url_or_path(app, arg);
            }
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .register_uri_scheme_protocol("syn-asset", |ctx, request| {
            asset_protocol::handle(ctx.app_handle(), request)
        })
        .manage(AppState::default())
        .setup(|app| {
            // Register the persisted quick-capture global shortcut.
            if let Ok(dir) = app.path().app_config_dir() {
                let hotkey = vault::config::load_config(&dir).quick_capture_hotkey;
                if let Err(err) = windows::register_quick_capture_shortcut(app.handle(), &hotkey) {
                    eprintln!("quick-capture shortcut: {err}");
                }
            }
            // Deep links delivered while running.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        deep_links::handle_url_or_path(&handle, url.as_str());
                    }
                });
                #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
                {
                    let _ = app.deep_link().register_all();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_platform,
            app_renderer_ready,
            vault_cmds::vault_get_current,
            vault_cmds::vault_list_local,
            vault_cmds::vault_open_local,
            vault_cmds::vault_close,
            vault_cmds::vault_pick,
            vault_cmds::vault_select_path,
            vault_cmds::vault_read_note,
            vault_cmds::vault_list_notes,
            vault_cmds::vault_list_folders,
            vault_cmds::vault_write_note,
            vault_cmds::vault_append_note,
            vault_cmds::vault_create_note,
            vault_cmds::vault_rename_note,
            vault_cmds::vault_delete_note,
            vault_cmds::vault_move_to_trash,
            vault_cmds::vault_restore_from_trash,
            vault_cmds::vault_empty_trash,
            vault_cmds::vault_archive_note,
            vault_cmds::vault_unarchive_note,
            vault_cmds::vault_duplicate_note,
            vault_cmds::vault_move_note,
            vault_cmds::vault_create_folder,
            vault_cmds::vault_rename_folder,
            vault_cmds::vault_delete_folder,
            vault_cmds::vault_duplicate_folder,
            vault_cmds::vault_text_search_capabilities,
            vault_cmds::vault_search_text,
            vault_cmds::vault_has_assets_dir,
            vault_cmds::vault_list_assets,
            vault_cmds::vault_rename_asset,
            vault_cmds::vault_move_asset,
            vault_cmds::vault_duplicate_asset,
            vault_cmds::vault_delete_asset,
            vault_cmds::vault_restore_deleted_asset,
            vault_cmds::vault_import_files,
            vault_cmds::vault_import_pasted_image,
            vault_cmds::vault_scan_tasks,
            vault_cmds::vault_scan_tasks_for,
            vault_cmds::vault_read_comments,
            vault_cmds::vault_write_comments,
            vault_cmds::vault_get_settings,
            vault_cmds::vault_set_settings,
            vault_cmds::vault_list_templates,
            vault_cmds::vault_read_template,
            vault_cmds::vault_write_template,
            vault_cmds::vault_delete_template,
            vault_cmds::vault_generate_demo_tour,
            vault_cmds::vault_remove_demo_tour,
            window_cmds::window_open_note,
            window_cmds::window_open_vault,
            window_cmds::window_toggle_quick_capture,
            window_cmds::app_get_quick_capture_hotkey,
            window_cmds::app_set_quick_capture_hotkey,
            window_cmds::app_get_quick_capture_pinned,
            window_cmds::app_set_quick_capture_pinned,
            window_cmds::app_open_markdown_file,
            window_cmds::app_read_external_file,
            window_cmds::app_write_external_file,
            window_cmds::app_move_external_file_to_vault,
            os_cmds::app_zoom_in,
            os_cmds::app_zoom_out,
            os_cmds::app_zoom_reset,
            os_cmds::app_list_fonts,
            os_cmds::app_icon_data_url,
            os_cmds::vault_reveal_note,
            os_cmds::vault_reveal_note_target,
            os_cmds::vault_reveal_folder,
            os_cmds::vault_reveal_folder_target,
            os_cmds::vault_reveal_assets_dir,
            workspace_cmds::workspace_list_remote_profiles,
            workspace_cmds::workspace_save_remote_profile,
            workspace_cmds::workspace_delete_remote_profile,
            workspace_cmds::workspace_connect_remote,
            workspace_cmds::workspace_connect_remote_profile,
            workspace_cmds::workspace_browse_server_directories,
        ])
        .build(tauri::generate_context!())
        .expect("error while building ZenNotes-rs")
        .run(|_app, _event| {
            // macOS/iOS deliver "open file"/"open URL" (file associations and
            // deep links) through the Opened run event. That variant only
            // exists on Apple platforms; on Linux/Windows these arrive via the
            // deep-link plugin + single-instance, so gate it by target.
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let tauri::RunEvent::Opened { urls } = _event {
                for url in urls {
                    deep_links::handle_url_or_path(_app, url.as_str());
                }
            }
        });
}
