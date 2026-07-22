//! Vault selection / config commands (M1). Mirrors the
//! `vault:get-current/list-local/open-local/close/pick/select-path`
//! handlers in apps/desktop/src/main/index.ts.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::ipc::types::{
    AssetMeta, CustomTemplateFile, DeletedAsset, FolderEntry, ImportedAsset, LocalVaultEntry,
    NoteComment, NoteContent, NoteMeta, VaultDemoTourResult, VaultInfo, VaultSettings, VaultTask,
    VaultTextSearchCapabilities, VaultTextSearchMatch, WriteTemplateInput,
};
use crate::search;
use crate::state::AppState;
use crate::vault::assets;
use crate::vault::comments;
use crate::vault::config;
use crate::vault::crud;
use crate::vault::demo_tour;
use crate::vault::folders;
use crate::vault::layout;
use crate::vault::listing;
use crate::vault::notes;
use crate::vault::settings;
use crate::vault::tasks;
use crate::vault::templates;

/// Absolute root of the currently open vault, or an error when none is open.
fn require_root(state: &AppState) -> Result<PathBuf, String> {
    state
        .current_root()
        .ok_or_else(|| "No vault is currently open".to_string())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))
}

/// Open `root` as the active local vault: ensure layout, update state, persist.
fn open_vault(
    app: &AppHandle,
    state: &AppState,
    root: &Path,
    opened_at: i64,
) -> Result<VaultInfo, String> {
    layout::ensure_vault_layout(root).map_err(|e| format!("Failed to prepare vault: {e}"))?;
    let resolved = PathBuf::from(config::resolve_path(&root.to_string_lossy()));
    let vault = layout::vault_info(&resolved);
    state.set_current(Some(vault.clone()));

    // (Re)start the filesystem watcher for the newly active vault.
    match crate::watcher::spawn(app.clone(), resolved.clone()) {
        Ok(debouncer) => state.set_watcher(Some(debouncer)),
        Err(err) => {
            eprintln!("vault watcher failed to start: {err}");
            state.set_watcher(None);
        }
    }

    let dir = config_dir(app)?;
    let v = vault.clone();
    config::update_config(&dir, move |cfg| {
        cfg.workspace_mode = "local".into();
        cfg.vault_root = Some(v.root.clone());
        cfg.local_vaults = config::remember_local_vault(&cfg.local_vaults, &v, opened_at);
    })
    .map_err(|e| format!("Failed to persist config: {e}"))?;

    Ok(vault)
}

/// v2.15 `openVaultWindow(root?)` support: open `root` as the active vault
/// (used by window_cmds before spawning the new workspace window).
pub(crate) fn open_local_vault_root(
    app: &AppHandle,
    state: &AppState,
    root: &str,
) -> Result<VaultInfo, String> {
    open_vault(app, state, Path::new(root), now_ms())
}

#[tauri::command]
pub fn vault_get_current(
    app: AppHandle,
    state: State<AppState>,
) -> Result<Option<VaultInfo>, String> {
    if let Some(v) = state.current() {
        return Ok(Some(v));
    }
    let dir = config_dir(&app)?;
    let cfg = config::load_config(&dir);
    let Some(root) = cfg.vault_root else {
        return Ok(None);
    };
    let vault = open_vault(&app, &state, Path::new(&root), now_ms())?;
    Ok(Some(vault))
}

#[tauri::command]
pub fn vault_list_local(app: AppHandle) -> Result<Vec<LocalVaultEntry>, String> {
    let dir = config_dir(&app)?;
    let cfg = config::load_config(&dir);
    let mut entries = config::local_vaults_to_entries(&cfg.local_vaults);
    if let Some(root) = cfg.vault_root {
        let resolved = config::resolve_path(&root);
        if !entries.iter().any(|e| config::resolve_path(&e.root) == resolved) {
            entries.insert(
                0,
                LocalVaultEntry {
                    name: config::basename(&resolved),
                    root: resolved,
                    last_opened_at: 0,
                },
            );
        }
    }
    Ok(entries)
}

#[tauri::command]
pub fn vault_open_local(
    app: AppHandle,
    state: State<AppState>,
    root: String,
) -> Result<Option<VaultInfo>, String> {
    let trimmed = root.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let vault = open_vault(&app, &state, Path::new(trimmed), now_ms())?;
    Ok(Some(vault))
}

#[tauri::command]
pub fn vault_close(
    app: AppHandle,
    state: State<AppState>,
) -> Result<Option<VaultInfo>, String> {
    let Some(current) = state.current() else {
        return Ok(None);
    };
    let dir = config_dir(&app)?;
    let cfg = config::load_config(&dir);
    let remaining = config::forget_local_vault(&cfg.local_vaults, &current.root);
    let next_root = remaining.first().map(|e| e.root.clone());

    let next_vault = match &next_root {
        Some(root) => Some(open_vault(&app, &state, Path::new(root), now_ms())?),
        None => {
            state.set_current(None);
            state.set_watcher(None);
            None
        }
    };

    // `open_vault` already re-persisted when a next vault exists; otherwise we
    // must clear the active root and drop the closed vault from the list.
    if next_vault.is_none() {
        config::update_config(&dir, move |cfg| {
            cfg.workspace_mode = "local".into();
            cfg.vault_root = None;
            cfg.local_vaults = config::forget_local_vault(&cfg.local_vaults, &current.root);
        })
        .map_err(|e| format!("Failed to persist config: {e}"))?;
    } else {
        // Drop the closed vault from the remembered list (the next vault was
        // just remembered with a fresh timestamp by open_vault).
        let closed_root = current.root.clone();
        config::update_config(&dir, move |cfg| {
            cfg.local_vaults = config::forget_local_vault(&cfg.local_vaults, &closed_root);
        })
        .map_err(|e| format!("Failed to persist config: {e}"))?;
    }

    Ok(next_vault)
}

#[tauri::command]
pub async fn vault_pick(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<VaultInfo>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose a vault folder")
        .blocking_pick_folder();
    let Some(file_path) = picked else {
        return Ok(None);
    };
    let path = file_path
        .into_path()
        .map_err(|e| format!("Invalid folder selection: {e}"))?;
    let vault = open_vault(&app, &state, &path, now_ms())?;
    Ok(Some(vault))
}

#[tauri::command]
pub fn vault_select_path(_target_path: String) -> Result<VaultInfo, String> {
    // Remote-workspace path selection arrives in M14.
    Err("Remote workspaces are not available in this build yet.".into())
}

// ---- M2: read / list -----------------------------------------------------

#[tauri::command]
pub fn vault_read_note(state: State<AppState>, rel_path: String) -> Result<NoteContent, String> {
    let root = require_root(&state)?;
    notes::read_note(&root, &rel_path)
}

#[tauri::command]
pub fn vault_list_notes(state: State<AppState>) -> Result<Vec<NoteMeta>, String> {
    let root = require_root(&state)?;
    Ok(listing::list_notes(&root))
}

#[tauri::command]
pub fn vault_list_folders(state: State<AppState>) -> Result<Vec<FolderEntry>, String> {
    let root = require_root(&state)?;
    Ok(listing::list_folders(&root))
}

// ---- M3: note CRUD -------------------------------------------------------

#[tauri::command]
pub fn vault_write_note(state: State<AppState>, rel_path: String, body: String) -> Result<NoteMeta, String> {
    crud::write_note(&require_root(&state)?, &rel_path, &body)
}

#[tauri::command]
pub fn vault_append_note(
    state: State<AppState>,
    rel_path: String,
    body: String,
    position: String,
) -> Result<NoteMeta, String> {
    crud::append_to_note(&require_root(&state)?, &rel_path, &body, &position)
}

#[tauri::command]
pub fn vault_create_note(
    state: State<AppState>,
    folder: String,
    title: Option<String>,
    subpath: Option<String>,
) -> Result<NoteMeta, String> {
    crud::create_note(
        &require_root(&state)?,
        &folder,
        title.as_deref(),
        subpath.as_deref().unwrap_or(""),
    )
}

#[tauri::command]
pub fn vault_rename_note(state: State<AppState>, rel_path: String, next_title: String) -> Result<NoteMeta, String> {
    crud::rename_note(&require_root(&state)?, &rel_path, &next_title)
}

#[tauri::command]
pub fn vault_delete_note(state: State<AppState>, rel_path: String) -> Result<(), String> {
    crud::delete_note(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_move_to_trash(state: State<AppState>, rel_path: String) -> Result<NoteMeta, String> {
    crud::move_to_trash(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_restore_from_trash(state: State<AppState>, rel_path: String) -> Result<NoteMeta, String> {
    crud::restore_from_trash(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_empty_trash(state: State<AppState>) -> Result<(), String> {
    crud::empty_trash(&require_root(&state)?)
}

#[tauri::command]
pub fn vault_archive_note(state: State<AppState>, rel_path: String) -> Result<NoteMeta, String> {
    crud::archive_note(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_unarchive_note(state: State<AppState>, rel_path: String) -> Result<NoteMeta, String> {
    crud::unarchive_note(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_duplicate_note(state: State<AppState>, rel_path: String) -> Result<NoteMeta, String> {
    crud::duplicate_note(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_move_note(
    state: State<AppState>,
    rel_path: String,
    target_folder: String,
    target_subpath: String,
) -> Result<NoteMeta, String> {
    crud::move_note(&require_root(&state)?, &rel_path, &target_folder, &target_subpath)
}

// ---- M5: folders ---------------------------------------------------------

#[tauri::command]
pub fn vault_create_folder(state: State<AppState>, folder: String, subpath: String) -> Result<(), String> {
    folders::create_folder(&require_root(&state)?, &folder, &subpath)
}

#[tauri::command]
pub fn vault_rename_folder(
    state: State<AppState>,
    folder: String,
    old_subpath: String,
    new_subpath: String,
) -> Result<String, String> {
    folders::rename_folder(&require_root(&state)?, &folder, &old_subpath, &new_subpath)
}

#[tauri::command]
pub fn vault_delete_folder(state: State<AppState>, folder: String, subpath: String) -> Result<(), String> {
    folders::delete_folder(&require_root(&state)?, &folder, &subpath)
}

#[tauri::command]
pub fn vault_duplicate_folder(state: State<AppState>, folder: String, subpath: String) -> Result<String, String> {
    folders::duplicate_folder(&require_root(&state)?, &folder, &subpath)
}

// ---- M6: full-text search ------------------------------------------------

fn tool_paths(paths: Option<serde_json::Value>) -> search::ToolPaths {
    let get = |key: &str| {
        paths
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    search::ToolPaths {
        ripgrep: get("ripgrepPath"),
        fzf: get("fzfPath"),
    }
}

#[tauri::command]
pub fn vault_text_search_capabilities(
    paths: Option<serde_json::Value>,
) -> VaultTextSearchCapabilities {
    let (ripgrep, fzf) = search::capabilities(&tool_paths(paths));
    VaultTextSearchCapabilities { ripgrep, fzf }
}

#[tauri::command]
pub fn vault_search_text(
    state: State<AppState>,
    query: String,
    backend: Option<String>,
    paths: Option<serde_json::Value>,
) -> Result<Vec<VaultTextSearchMatch>, String> {
    let root = require_root(&state)?;
    Ok(search::search_vault_text(
        &root,
        &query,
        backend.as_deref().unwrap_or("auto"),
        &tool_paths(paths),
    ))
}

// ---- M7: assets ----------------------------------------------------------

#[tauri::command]
pub fn vault_has_assets_dir(state: State<AppState>) -> Result<bool, String> {
    Ok(assets::has_assets_dir(&require_root(&state)?))
}

#[tauri::command]
pub fn vault_list_assets(state: State<AppState>) -> Result<Vec<AssetMeta>, String> {
    Ok(assets::list_assets(&require_root(&state)?))
}

#[tauri::command]
pub fn vault_rename_asset(state: State<AppState>, rel_path: String, next_name: String) -> Result<AssetMeta, String> {
    assets::rename_asset(&require_root(&state)?, &rel_path, &next_name)
}

#[tauri::command]
pub fn vault_move_asset(state: State<AppState>, rel_path: String, target_dir: String) -> Result<AssetMeta, String> {
    assets::move_asset(&require_root(&state)?, &rel_path, &target_dir)
}

#[tauri::command]
pub fn vault_duplicate_asset(state: State<AppState>, rel_path: String) -> Result<AssetMeta, String> {
    assets::duplicate_asset(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_delete_asset(state: State<AppState>, rel_path: String) -> Result<DeletedAsset, String> {
    assets::delete_asset(&require_root(&state)?, &rel_path)
}

#[tauri::command]
pub fn vault_restore_deleted_asset(state: State<AppState>, asset: DeletedAsset) -> Result<AssetMeta, String> {
    assets::restore_deleted_asset(&require_root(&state)?, &asset)
}

#[tauri::command]
pub fn vault_import_files(
    state: State<AppState>,
    note_path: String,
    source_paths: Vec<String>,
) -> Result<Vec<ImportedAsset>, String> {
    assets::import_files(&require_root(&state)?, &note_path, &source_paths)
}

#[tauri::command]
pub fn vault_import_pasted_image(
    state: State<AppState>,
    data: Vec<u8>,
    mime_type: String,
    suggested_name: Option<String>,
) -> Result<ImportedAsset, String> {
    assets::import_pasted_image(&require_root(&state)?, &data, &mime_type, suggested_name.as_deref())
}

// ---- M8: tasks -----------------------------------------------------------

#[tauri::command]
pub fn vault_scan_tasks(state: State<AppState>) -> Result<Vec<VaultTask>, String> {
    Ok(tasks::scan_all_tasks(&require_root(&state)?))
}

#[tauri::command]
pub fn vault_scan_tasks_for(state: State<AppState>, rel_path: String) -> Result<Vec<VaultTask>, String> {
    Ok(tasks::scan_tasks_for_path(&require_root(&state)?, &rel_path))
}

// ---- M9: comments --------------------------------------------------------

#[tauri::command]
pub fn vault_read_comments(state: State<AppState>, rel_path: String) -> Result<Vec<NoteComment>, String> {
    Ok(comments::read_note_comments(&require_root(&state)?, &rel_path))
}

#[tauri::command]
pub fn vault_write_comments(
    state: State<AppState>,
    rel_path: String,
    comments: serde_json::Value,
) -> Result<Vec<NoteComment>, String> {
    crate::vault::comments::write_note_comments(&require_root(&state)?, &rel_path, &comments)
}

// ---- M10: settings + templates + demo tour -------------------------------

#[tauri::command]
pub fn vault_get_settings(state: State<AppState>) -> Result<VaultSettings, String> {
    Ok(settings::get_vault_settings(&require_root(&state)?))
}

#[tauri::command]
pub fn vault_set_settings(state: State<AppState>, next: serde_json::Value) -> Result<VaultSettings, String> {
    settings::set_vault_settings(&require_root(&state)?, &next)
}

#[tauri::command]
pub fn vault_list_templates(state: State<AppState>) -> Result<Vec<CustomTemplateFile>, String> {
    Ok(templates::list_custom_templates(&require_root(&state)?))
}

#[tauri::command]
pub fn vault_read_template(state: State<AppState>, source_path: String) -> Result<String, String> {
    templates::read_custom_template(&require_root(&state)?, &source_path)
}

#[tauri::command]
pub fn vault_write_template(state: State<AppState>, input: WriteTemplateInput) -> Result<CustomTemplateFile, String> {
    templates::write_custom_template(&require_root(&state)?, &input)
}

#[tauri::command]
pub fn vault_delete_template(state: State<AppState>, source_path: String) -> Result<(), String> {
    templates::delete_custom_template(&require_root(&state)?, &source_path)
}

#[tauri::command]
pub fn vault_generate_demo_tour(state: State<AppState>) -> Result<VaultDemoTourResult, String> {
    demo_tour::generate_demo_tour(&require_root(&state)?)
}

#[tauri::command]
pub fn vault_remove_demo_tour(state: State<AppState>) -> Result<VaultDemoTourResult, String> {
    demo_tour::remove_demo_tour(&require_root(&state)?)
}
