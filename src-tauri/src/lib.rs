//! ZenNotes-rs — Tauri (Rust) backend.
//!
//! Reimplements the ZenNotes Electron `main/` process as Tauri commands.
//! The frontend talks to this backend through the same ZenBridge contract
//! it used with Electron; each Electron IPC channel maps to a `#[command]`
//! here (see src/bridge/tauri-bridge.ts on the frontend side).

mod app_config;
mod app_info;
mod asset_protocol;
mod code_languages;
mod custom_css;
mod deep_links;
mod external_urls;
mod ipc;
mod link_metadata;
mod mcp;
mod os;
mod remote;
mod search;
mod secrets;
mod standalone_links;
mod state;
mod tray;
mod undo_history;
mod unsplash;
mod vault;
mod watcher;
mod windows;

use ipc::{mcp_cmds, os_cmds, vault_cmds, window_cmds, workspace_cmds};
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

/// `zennotes-rs mcp`: see `mcp::run`.
pub fn run_mcp(args: &[String]) -> i32 {
    mcp::run(args)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // single-instance must be registered first; it forwards file/deep-link
        // args from a second launch to the running instance.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let args = || argv.iter().skip(1);
            // A compositor keybind asking for Quick Capture should not raise the
            // main window as well (Wayland has no global hotkeys for us to grab).
            if args().any(|arg| deep_links::is_quick_capture_request(arg)) {
                // The deep-link plugin below already delivers the URL form.
                if args().any(|arg| !deep_links::is_deep_link(arg) && deep_links::is_quick_capture_request(arg)) {
                    let _ = windows::toggle_quick_capture(app, windows::read_pinned(app));
                }
                return;
            }
            // Deep links reach `on_open_url` through the plugin's own handling
            // of a second launch; handling them here too would run them twice.
            for arg in args().filter(|arg| !deep_links::is_deep_link(arg)) {
                deep_links::handle_url_or_path(app, arg);
            }
            // Focuses the main window, and shows any the tray is holding (#947).
            tray::on_reopen(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // Upstream's `will-navigate`: every navigation in every window.
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("navigation-guard")
                .on_navigation(|webview, url| {
                    external_urls::guard_navigation(webview.app_handle(), url.as_str())
                })
                .build(),
        )
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .register_uri_scheme_protocol("syn-asset", |ctx, request| {
            asset_protocol::handle(ctx.app_handle(), request)
        })
        .register_uri_scheme_protocol("zen-theme", |_ctx, request| {
            custom_css::handle_theme_asset(request)
        })
        .manage(AppState::default())
        .manage(tray::TrayState::default())
        .on_window_event(tray::on_window_event)
        .setup(|app| {
            // Saved undo histories (#793) expire and are capped per vault; off
            // the boot path, so a slow sweep never delays the first window.
            undo_history::spawn_prune(app.handle());
            // Files a macOS or Windows drop staged during the last session.
            os_cmds::clear_staged_drops();
            // Put the tray icon back when "Keep running in the tray" was left on.
            tray::restore_at_launch(app.handle());
            // Watch the portable config.toml for external edits (synced
            // dotfiles / hand-edits) and push changes to every window.
            match app_config::spawn_watcher(app.handle().clone()) {
                Ok(debouncer) => {
                    app.manage(app_config::PortableConfigWatcher(std::sync::Mutex::new(
                        Some(debouncer),
                    )));
                }
                Err(err) => eprintln!("config watcher failed to start: {err}"),
            }
            // Watch the custom-themes + overrides dirs for edits.
            match custom_css::spawn_watchers(app.handle().clone()) {
                Ok(debouncers) => {
                    app.manage(custom_css::CustomCssWatchers(std::sync::Mutex::new(
                        debouncers,
                    )));
                }
                Err(err) => eprintln!("custom-css watchers failed to start: {err}"),
            }
            // Watch the installed code-languages dir for edits.
            match code_languages::spawn_watcher(app.handle().clone()) {
                Ok(debouncers) => {
                    app.manage(code_languages::CodeLanguageWatchers(std::sync::Mutex::new(
                        debouncers,
                    )));
                }
                Err(err) => eprintln!("code-languages watcher failed to start: {err}"),
            }
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
            // First launch through `zennotes-rs --quick-capture`.
            if std::env::args().skip(1).any(|arg| deep_links::is_quick_capture_request(&arg)) {
                let _ = windows::toggle_quick_capture(app.handle(), windows::read_pinned(app.handle()));
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
            vault_cmds::vault_task_sources,
            vault_cmds::vault_task_source_for,
            vault_cmds::db_read_texts,
            vault_cmds::vault_read_comments,
            vault_cmds::vault_write_comments,
            vault_cmds::vault_get_settings,
            vault_cmds::vault_set_settings,
            vault_cmds::vault_list_templates,
            vault_cmds::vault_read_template,
            vault_cmds::vault_write_template,
            vault_cmds::vault_delete_template,
            vault_cmds::db_ensure_vault_layout,
            vault_cmds::db_remove_dir_if_empty,
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
            mcp_cmds::mcp_get_runtime,
            mcp_cmds::mcp_get_statuses,
            mcp_cmds::mcp_install,
            mcp_cmds::mcp_uninstall,
            mcp_cmds::mcp_get_instructions,
            mcp_cmds::mcp_set_instructions,
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
            os_cmds::vault_reveal_file_path,
            os_cmds::vault_open_external_file,
            os_cmds::devtools_toggle,
            os_cmds::app_clipboard_write_text,
            os_cmds::app_dropped_folders,
            os_cmds::app_stage_dropped_file,
            os_cmds::app_open_vault_asset_url,
            app_config::config_file_path,
            app_config::config_file_read,
            app_config::config_file_write,
            custom_css::custom_themes_scan,
            custom_css::custom_themes_dir_path,
            custom_css::custom_themes_reveal,
            custom_css::custom_themes_delete,
            custom_css::custom_themes_reserve,
            custom_css::custom_themes_write_files,
            custom_css::overrides_list,
            custom_css::overrides_reveal,
            custom_css::overrides_delete,
            vault_cmds::workspace_state_read,
            vault_cmds::workspace_state_write,
            vault_cmds::vault_root_content_hidden,
            vault_cmds::vault_create_excalidraw,
            vault_cmds::vault_list_deleted_assets,
            vault_cmds::vault_purge_deleted_asset,
            vault_cmds::vault_empty_deleted_assets,
            vault_cmds::db_read_text,
            vault_cmds::db_write_text,
            vault_cmds::db_exists,
            vault_cmds::db_mkdir,
            vault_cmds::db_rename_form_dir,
            vault_cmds::db_folder_root_rel,
            vault_cmds::db_create_record_page,
            vault_cmds::vault_write_drawing,
            vault_cmds::app_open_folder_temporary,
            os_cmds::vault_fetch_link_metadata,
            os_cmds::unsplash_request,
            os_cmds::unsplash_key_is_set,
            os_cmds::unsplash_key_can_store,
            os_cmds::unsplash_key_set,
            os_cmds::unsplash_key_clear,
            workspace_cmds::workspace_list_remote_profiles,
            workspace_cmds::workspace_save_remote_profile,
            workspace_cmds::workspace_delete_remote_profile,
            workspace_cmds::workspace_connect_remote,
            workspace_cmds::workspace_connect_remote_profile,
            workspace_cmds::workspace_browse_server_directories,
            // v2.45 mirror
            vault_cmds::vault_list_workflows,
            vault_cmds::vault_write_workflow,
            vault_cmds::vault_delete_workflow,
            vault_cmds::vault_export_workflow,
            vault_cmds::vault_import_workflow_file,
            vault_cmds::db_list_dir,
            vault_cmds::db_file_size,
            vault_cmds::db_append_text,
            vault_cmds::db_delete_file,
            vault_cmds::db_write_through_links,
            vault_cmds::db_remove_through_links,
            vault_cmds::vault_read_asset_base64,
            vault_cmds::vault_open_asset_externally,
            window_cmds::app_open_file_dialog,
            window_cmds::external_file_follow_link,
            code_languages::code_languages_scan,
            code_languages::code_languages_write,
            code_languages::code_languages_write_manifest,
            code_languages::code_languages_delete,
            code_languages::code_languages_reveal,
            // v2.73 mirror
            vault_cmds::vault_list_agent_edits,
            vault_cmds::vault_read_agent_edit,
            vault_cmds::vault_write_agent_edit,
            undo_history::undo_history_read,
            undo_history::undo_history_write,
            undo_history::undo_history_clear,
            external_urls::app_open_external_url,
            app_info::app_install_info,
            vault_cmds::db_link_text,
            vault_cmds::db_path_taken,
            vault_cmds::db_respell_link,
            vault_cmds::db_rename_entry,
            vault_cmds::db_remove_entry,
            vault_cmds::db_sidecar_rel,
            vault_cmds::db_prepare_note_creation,
            vault_cmds::db_prune_sidecar_dirs,
            tray::app_get_keep_running_in_tray,
            tray::app_set_keep_running_in_tray,
            tray::window_hide_to_tray,
            tray::app_quit,
            tray::app_tray_sync_status,
        ])
        .build(tauri::generate_context!())
        .expect("error while building ZenNotes-rs")
        .run(|_app, _event| {
            // macOS/iOS deliver "open file"/"open URL" (file associations and
            // deep links) through the Opened run event. That variant only
            // exists on Apple platforms; on Linux/Windows these arrive via the
            // deep-link plugin + single-instance, so gate it by target.
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let tauri::RunEvent::Opened { urls } = &_event {
                for url in urls {
                    deep_links::handle_url_or_path(_app, url.as_str());
                }
            }
            // A dock click must bring back a usable window, including the ones
            // the tray is holding.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                tray::on_reopen(_app);
            }
        });
}
