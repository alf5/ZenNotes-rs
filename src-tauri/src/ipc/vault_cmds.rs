//! Vault selection / config commands (M1). Mirrors the
//! `vault:get-current/list-local/open-local/close/pick/select-path`
//! handlers in apps/desktop/src/main/index.ts.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::ipc::types::{
    AssetMeta, CustomTemplateFile, DeletedAsset, FolderEntry, ImportedAsset,
    ImportedWorkflowFile, LocalVaultEntry, NoteComment, NoteContent, NoteMeta,
    VaultInfo, VaultSettings, VaultTextSearchCapabilities,
    VaultTextSearchMatch, WorkflowFile, WriteTemplateInput,
};
use crate::search;
use crate::state::AppState;
use crate::vault::assets;
use crate::vault::comments;
use crate::vault::config;
use crate::vault::crud;
use crate::vault::folders;
use crate::vault::layout;
use crate::vault::listing;
use crate::vault::notes;
use crate::vault::settings;
use crate::vault::sidecars;
use crate::vault::templates;
use crate::vault::workflows;

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
    let vault = settings::describe_vault(&resolved);
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

/// Port of `syncLocalVaultName` (#692): re-read the open vault's display
/// name and give the active vault and the remembered-vaults list the result.
/// After a settings save this makes a rename land everywhere at once; on
/// getCurrentVault and every listing it makes an edit to vault.json from
/// outside (a git pull, another machine) show up. Returns the vault in effect.
fn sync_local_vault_name(app: &AppHandle, state: &AppState) -> Option<VaultInfo> {
    let current = state.current()?;
    let name = settings::describe_vault(Path::new(&current.root)).name;
    let current = if current.name != name {
        let renamed = VaultInfo { name: name.clone(), ..current };
        state.set_current(Some(renamed.clone()));
        renamed
    } else {
        current
    };
    // A temporary session is never remembered, so it has no entry to rename.
    if current.temporary != Some(true) {
        if let Ok(dir) = config_dir(app) {
            let resolved = config::resolve_path(&current.root);
            let stale = config::load_config(&dir)
                .local_vaults
                .iter()
                .any(|e| config::resolve_path(&e.root) == resolved && e.name != name);
            if stale {
                let root = current.root.clone();
                let _ = config::update_config(&dir, move |cfg| {
                    cfg.local_vaults = config::rename_local_vault(&cfg.local_vaults, &root, &name);
                });
            }
        }
    }
    Some(current)
}

// ---- v2.15 phase A: workspace state, drawings, deleted assets -------------

/// Whether the active vault is an ephemeral "open folder temporarily"
/// session — those never get `.zennotes` state written into them.
fn active_vault_is_temporary(state: &AppState) -> bool {
    state.current().and_then(|v| v.temporary).unwrap_or(false)
}

/// `workspace-state:read` — raw `<vault>/.zennotes/workspace.json` contents.
/// Never parsed here; the renderer owns the schema. No vault → null, matching
/// upstream's remote/ephemeral null path.
#[tauri::command]
pub fn workspace_state_read(state: State<AppState>) -> Result<Option<String>, String> {
    if active_vault_is_temporary(&state) {
        return Ok(None);
    }
    let Ok(root) = require_root(&state) else {
        return Ok(None);
    };
    match std::fs::read_to_string(root.join(layout::INTERNAL_VAULT_DIR).join("workspace.json")) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("workspace state read failed: {e}")),
    }
}

/// `workspace-state:write` — persist the renderer's snapshot verbatim.
#[tauri::command]
pub fn workspace_state_write(state: State<AppState>, json: String) -> Result<(), String> {
    if active_vault_is_temporary(&state) {
        return Ok(());
    }
    let Ok(root) = require_root(&state) else {
        return Ok(());
    };
    let dir = root.join(layout::INTERNAL_VAULT_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    std::fs::write(dir.join("workspace.json"), json)
        .map_err(|e| format!("workspace state write failed: {e}"))
}

/// `vault:root-content-hidden` — true when the saved location says "inbox"
/// but a fresh scan would infer a root-mode vault (drives the "Switch to
/// Vault root" banner; upstream vault.ts:1241).
#[tauri::command]
pub fn vault_root_content_hidden(state: State<AppState>) -> Result<bool, String> {
    let Ok(root) = require_root(&state) else {
        return Ok(false);
    };
    if settings::get_vault_settings(&root).primary_notes_location != "inbox" {
        return Ok(false);
    }
    Ok(settings::infer_primary_notes_location(&root) == "root")
}

/// `vault:create-excalidraw` — new empty drawing (v2.15).
#[tauri::command]
pub fn vault_create_excalidraw(
    state: State<AppState>,
    folder: String,
    subpath: Option<String>,
    title: Option<String>,
) -> Result<NoteMeta, String> {
    let root = require_root(&state)?;
    crud::create_excalidraw(&root, &folder, title.as_deref(), subpath.as_deref().unwrap_or(""))
}

#[tauri::command]
pub fn vault_list_deleted_assets(state: State<AppState>) -> Result<Vec<DeletedAsset>, String> {
    let Ok(root) = require_root(&state) else {
        return Ok(Vec::new());
    };
    Ok(assets::list_deleted_assets(&root))
}

#[tauri::command]
pub fn vault_purge_deleted_asset(state: State<AppState>, undo_token: String) -> Result<(), String> {
    let root = require_root(&state)?;
    assets::purge_deleted_asset(&root, &undo_token)
}

#[tauri::command]
pub fn vault_empty_deleted_assets(state: State<AppState>) -> Result<(), String> {
    let root = require_root(&state)?;
    assets::empty_deleted_assets(&root);
    Ok(())
}

// ---- v2.15 cluster C: databases file layer --------------------------------
// The CSV/schema logic lives in src/bridge/databases.ts (vendored
// shared-domain functions run in the webview); these commands are the raw
// vault-scoped file IO underneath — same trust boundary as the note CRUD.

fn db_atomic_write(abs: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        let tmp = parent.join(format!(
            ".{}.{}.tmp",
            abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            std::process::id()
        ));
        std::fs::write(&tmp, text).map_err(|e| format!("write failed: {e}"))?;
        return std::fs::rename(&tmp, abs).map_err(|e| format!("rename failed: {e}"));
    }
    std::fs::write(abs, text).map_err(|e| format!("write failed: {e}"))
}

#[tauri::command]
pub fn db_read_text(state: State<AppState>, rel_path: String) -> Result<Option<String>, String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    match std::fs::read_to_string(&abs) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read failed: {e}")),
    }
}

#[tauri::command]
pub fn db_write_text(state: State<AppState>, rel_path: String, text: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    db_atomic_write(&abs, &text)
}

#[tauri::command]
pub fn db_exists(state: State<AppState>, rel_path: String) -> Result<bool, String> {
    let root = require_root(&state)?;
    Ok(notes::resolve_safe(&root, &rel_path)?.exists())
}

#[tauri::command]
pub fn db_mkdir(state: State<AppState>, rel_path: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    std::fs::create_dir_all(abs).map_err(|e| format!("mkdir failed: {e}"))
}

/// Rename a database's `<Name>.base` folder. Like upstream's renameDatabase,
/// a database inside a top folder goes through the folder rename (so its
/// comment tree, creation dates and agent edits follow, and folder icons and
/// colors keyed on it are rewritten); one outside every top folder (a
/// root-level database in an inbox-mode vault) moves its trees directly.
#[tauri::command]
pub fn db_rename_form_dir(state: State<AppState>, old_rel: String, new_rel: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let old_abs = notes::resolve_safe(&root, &old_rel)?;
    let new_abs = notes::resolve_safe(&root, &new_rel)?;
    if let Some(folder) = notes::folder_for_relative_path(&notes::to_posix(&old_rel)) {
        let top = layout::folder_root(&root, &folder);
        if let (Ok(old_sub), Ok(new_sub)) = (old_abs.strip_prefix(&top), new_abs.strip_prefix(&top)) {
            let (old_sub, new_sub) = (notes::to_posix(&old_sub.to_string_lossy()), notes::to_posix(&new_sub.to_string_lossy()));
            if !old_sub.is_empty() && !new_sub.is_empty() {
                return folders::rename_folder(&root, &folder, &old_sub, &new_sub).map(|_| ());
            }
        }
    }
    folders::rename_folder_trees(&root, &old_rel, &new_rel, || Ok(()))
}

/// Vault-relative root of a NoteFolder ("" when the primary notes area is the
/// vault root) — lets the frontend compute create locations like upstream's
/// `folderRoot` without knowing the layout rules.
#[tauri::command]
pub fn db_folder_root_rel(state: State<AppState>, folder: String) -> Result<String, String> {
    let root = require_root(&state)?;
    let abs = layout::folder_root(&root, &folder);
    let root_res = config::resolve_path(&root.to_string_lossy());
    let abs_res = config::resolve_path(&abs.to_string_lossy());
    let rel = abs_res
        .strip_prefix(&format!("{root_res}{}", std::path::MAIN_SEPARATOR))
        .unwrap_or("");
    Ok(notes::to_posix(rel))
}

#[tauri::command]
pub fn db_create_record_page(
    state: State<AppState>,
    form_dir_rel: String,
    title: String,
    body: String,
) -> Result<String, String> {
    let root = require_root(&state)?;
    crud::create_database_record_page(&root, &form_dir_rel, &title, &body)
}

/// `vault:convert-obsidian-excalidraw` write half: the frontend extracts the
/// scene (shared-domain, lz-string) and this persists it as a sibling
/// `.excalidraw`, returning the new file's meta.
#[tauri::command]
pub fn vault_write_drawing(
    state: State<AppState>,
    dir_rel: String,
    base_title: String,
    body: String,
) -> Result<NoteMeta, String> {
    let root = require_root(&state)?;
    crud::write_drawing_file(&root, &dir_rel, &base_title, &body)
}

/// `app:open-folder-temporary` — open a folder as an ephemeral session:
/// no vault scaffolding, no `.zennotes` writes, not remembered across
/// restarts (config.vaultRoot is left untouched). This backend keeps a
/// single active vault shared by all windows, so the session replaces the
/// active vault until the app restarts or another vault is opened; the
/// `VaultInfo.temporary` flag drives the renderer's banner.
#[tauri::command]
pub fn app_open_folder_temporary(
    app: AppHandle,
    state: State<AppState>,
    raw_path: String,
) -> Result<(), String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err("Folder path is required.".into());
    }
    let resolved = PathBuf::from(config::resolve_path(trimmed));
    let md = std::fs::metadata(&resolved).map_err(|e| format!("Not a readable folder: {e}"))?;
    if !md.is_dir() {
        return Err("Not a folder.".into());
    }
    // Already the active root → just focus.
    if let Some(current) = state.current() {
        if config::resolve_path(&current.root) == resolved.to_string_lossy() {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
            return Ok(());
        }
    }
    // An empty folder doesn't spawn a session (upstream parity).
    if !listing::folder_has_markdown(&resolved) {
        return Ok(());
    }
    let mut vault = settings::describe_vault(&resolved);
    vault.temporary = Some(true);
    state.set_current(Some(vault));
    match crate::watcher::spawn(app.clone(), resolved.clone()) {
        Ok(debouncer) => state.set_watcher(Some(debouncer)),
        Err(err) => {
            eprintln!("vault watcher failed to start: {err}");
            state.set_watcher(None);
        }
    }
    crate::windows::open_vault_window(&app, &state)?;
    Ok(())
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
    if let Some(v) = sync_local_vault_name(&app, &state) {
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
pub fn vault_list_local(
    app: AppHandle,
    state: State<AppState>,
) -> Result<Vec<LocalVaultEntry>, String> {
    // The open vault first: a display name that changed on disk (#692)
    // reaches the remembered list here, so the switcher and the sidebar
    // header, which read that list, never lag behind the vault they show.
    sync_local_vault_name(&app, &state);
    let dir = config_dir(&app)?;
    let cfg = config::load_config(&dir);
    let mut entries = config::local_vaults_to_entries(&cfg.local_vaults);
    if let Some(root) = cfg.vault_root {
        let resolved = config::resolve_path(&root);
        if !entries.iter().any(|e| config::resolve_path(&e.root) == resolved) {
            entries.insert(
                0,
                LocalVaultEntry {
                    name: settings::describe_vault(Path::new(&resolved)).name,
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
    crud::write_note(&require_root(&state)?, &rel_path, &body, active_vault_is_temporary(&state))
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
    crud::delete_note(&require_root(&state)?, &rel_path, active_vault_is_temporary(&state))
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
    folders::delete_folder(&require_root(&state)?, &folder, &subpath, active_vault_is_temporary(&state))
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

// ---- tasks: the file layer under src/bridge/tasks.ts ----------------------
// Parsing runs in the webview through vendored @shared/tasks, as upstream's
// main process does (apps/desktop/src/main/tasks.ts): the rules a task is
// read by stay one definition. These commands only say which notes are live
// and hand over their text.

/// A note Tasks may read: its path, title and top folder.
#[derive(serde::Serialize)]
pub struct TaskSource {
    path: String,
    title: String,
    folder: String,
}

const LIVE_FOLDERS: [&str; 3] = ["inbox", "quick", "archive"];

fn task_source(rel: String, folder: String) -> Option<TaskSource> {
    // Drawings are JSON, never task lists; trash never feeds Tasks.
    if !LIVE_FOLDERS.contains(&folder.as_str()) || notes::is_excalidraw_path(&rel) {
        return None;
    }
    let title = notes::title_from_path(Path::new(&rel));
    Some(TaskSource { path: rel, title, folder })
}

/// Every live note, without reading any of them.
#[tauri::command]
pub fn vault_task_sources(state: State<AppState>) -> Result<Vec<TaskSource>, String> {
    let root = require_root(&state)?;
    Ok(listing::list_note_paths(&root)
        .into_iter()
        .filter_map(|(rel, folder)| task_source(rel, folder))
        .collect())
}

/// One note, when it is live (a single-note rescan after an edit).
#[tauri::command]
pub fn vault_task_source_for(state: State<AppState>, rel_path: String) -> Result<Option<TaskSource>, String> {
    require_root(&state)?;
    let posix = notes::to_posix(&rel_path);
    Ok(notes::folder_for_relative_path(&posix).and_then(|folder| task_source(posix, folder)))
}

/// Many vault files' text in one round trip; None for any that is missing
/// or unreadable.
#[tauri::command]
pub fn db_read_texts(state: State<AppState>, rel_paths: Vec<String>) -> Result<Vec<Option<String>>, String> {
    let root = require_root(&state)?;
    Ok(rel_paths
        .iter()
        .map(|rel| notes::resolve_safe(&root, rel).ok().and_then(|abs| std::fs::read_to_string(abs).ok()))
        .collect())
}

// ---- agent edit records (docs/ideas/agent-edit-review.md) -----------------
// The file layer of apps/desktop/src/main/agent-edit-files.ts. Records are
// written by an assistant editing through upstream's MCP server (this build
// ships none, but the vault is shared with it); the webview parses and
// validates them with vendored @shared/agent-edits.

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEditFile {
    note_path: String,
    text: String,
}

#[tauri::command]
pub fn vault_list_agent_edits(state: State<AppState>) -> Result<Vec<AgentEditFile>, String> {
    let Ok(root) = require_root(&state) else { return Ok(Vec::new()) };
    Ok(sidecars::list_agent_edit_texts(&root)
        .into_iter()
        .map(|(note_path, text)| AgentEditFile { note_path, text })
        .collect())
}

#[tauri::command]
pub fn vault_read_agent_edit(state: State<AppState>, rel_path: String) -> Result<Option<String>, String> {
    let Ok(root) = require_root(&state) else { return Ok(None) };
    Ok(sidecars::read_agent_edit_text(&root, &rel_path))
}

/// Save a review's progress (an already validated, serialized record), or
/// delete the record with None. A temporary folder session gets no state.
#[tauri::command]
pub fn vault_write_agent_edit(state: State<AppState>, rel_path: String, text: Option<String>) -> Result<(), String> {
    let root = require_root(&state)?;
    if active_vault_is_temporary(&state) {
        return Ok(());
    }
    sidecars::write_agent_edit_text(&root, &rel_path, text.as_deref())
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
pub fn vault_set_settings(
    app: AppHandle,
    state: State<AppState>,
    next: serde_json::Value,
) -> Result<VaultSettings, String> {
    let root = require_root(&state)?;
    // Ephemeral session: apply for this run but never write .zennotes state
    // into a temporarily-opened folder (upstream ephemeral-vaults contract).
    if active_vault_is_temporary(&state) {
        let fallback = settings::infer_primary_notes_location(&root);
        return Ok(settings::normalize_vault_settings(Some(&next), &fallback));
    }
    let saved = settings::set_vault_settings(&root, &next)?;
    // The display name may have moved with this save (#692); the active vault
    // and the remembered list follow before the renderer hears back.
    sync_local_vault_name(&app, &state);
    Ok(saved)
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

/// Scaffold the vault's folders (`ensureVaultLayout`), for the demo tour,
/// which places its notes from vendored data in src/bridge/demo-tour.ts.
#[tauri::command]
pub fn db_ensure_vault_layout(state: State<AppState>) -> Result<(), String> {
    layout::ensure_vault_layout(&require_root(&state)?).map_err(|e| format!("layout failed: {e}"))
}

/// Remove a vault directory only when it is empty (`removeDirIfEmpty`).
#[tauri::command]
pub fn db_remove_dir_if_empty(state: State<AppState>, rel_path: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    if std::fs::read_dir(&abs).map(|mut d| d.next().is_none()).unwrap_or(false) {
        let _ = std::fs::remove_dir(&abs);
    }
    Ok(())
}

// ---- v2.45 M18: workflows -------------------------------------------------
// File layer only. The applier runs in src/bridge/workflows.ts over the
// generic `db_*` primitives above, because every op transform it needs is
// already vendored in `@shared/workflows/apply-ops` — the same split the CSV
// databases use.

#[tauri::command]
pub fn vault_list_workflows(state: State<AppState>) -> Result<Vec<WorkflowFile>, String> {
    Ok(workflows::list_workflow_files(&require_root(&state)?))
}

#[tauri::command]
pub fn vault_write_workflow(
    state: State<AppState>,
    slug: String,
    raw: String,
    previous_source_path: Option<String>,
) -> Result<WorkflowFile, String> {
    workflows::write_workflow_file(
        &require_root(&state)?,
        &slug,
        &raw,
        previous_source_path.as_deref(),
    )
}

#[tauri::command]
pub fn vault_delete_workflow(state: State<AppState>, source_path: String) -> Result<(), String> {
    workflows::delete_workflow_file(&require_root(&state)?, &source_path)
}

/// Save a copy of a workflow file anywhere on disk, through a native save
/// dialog. `None` when the dialog was cancelled.
#[tauri::command]
pub async fn vault_export_workflow(
    app: AppHandle,
    suggested_name: String,
    raw: String,
) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Export workflow")
        .set_file_name(workflows::safe_export_filename(&suggested_name))
        .add_filter("Markdown", &["md"])
        .blocking_save_file();
    let Some(file_path) = picked else {
        return Ok(None);
    };
    let path = file_path
        .into_path()
        .map_err(|e| format!("Invalid save location: {e}"))?;
    std::fs::write(&path, raw).map_err(|e| format!("write failed: {e}"))?;
    Ok(Some(path.to_string_lossy().to_string()))
}

/// Read a workflow file the user picks. Reading is ALL this does: the bytes
/// stay untrusted until `@shared/workflows/share` has reviewed them in the
/// renderer, and nothing reaches `.zennotes/workflows` except through
/// `vault_write_workflow` afterwards.
#[tauri::command]
pub async fn vault_import_workflow_file(
    app: AppHandle,
) -> Result<Option<ImportedWorkflowFile>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Import workflow")
        .add_filter("Markdown", &["md"])
        .blocking_pick_file();
    let Some(file_path) = picked else {
        return Ok(None);
    };
    let path = file_path
        .into_path()
        .map_err(|e| format!("Invalid file selection: {e}"))?;
    workflows::read_workflow_import_file(&path).map(Some)
}

// ---- v2.45: generic vault file primitives ---------------------------------
// Used by the TypeScript workflow applier for its journal and run ledgers.
// Same trust boundary as the `db_*` commands: every path goes through
// `notes::resolve_safe`, so nothing addresses outside the open vault.

/// File names directly inside a vault-relative directory (no recursion, no
/// directories). A missing directory reads as empty, which is the normal case
/// for a vault that has never run a workflow.
#[tauri::command]
pub fn db_list_dir(state: State<AppState>, rel_path: String) -> Result<Vec<String>, String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    let Ok(entries) = std::fs::read_dir(&abs) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    Ok(names)
}

/// Size in bytes, or 0 when the file is missing — the callers are pruning by
/// total size, where an absent file contributing nothing is the right answer.
#[tauri::command]
pub fn db_file_size(state: State<AppState>, rel_path: String) -> Result<u64, String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    Ok(std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0))
}

/// Append a line to a file, creating it and its parents if needed. The crash
/// journal is written this way rather than through `db_write_text`: rewriting
/// the whole journal per entry is quadratic, and an append is also the only
/// form where a killed process leaves every completed entry intact.
#[tauri::command]
pub fn db_append_text(state: State<AppState>, rel_path: String, text: String) -> Result<(), String> {
    use std::io::Write;
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&abs)
        .map_err(|e| format!("open failed: {e}"))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("write failed: {e}"))?;
    // The journal's whole job is to survive a kill, which it only does once the
    // bytes are on the device rather than in the page cache.
    file.sync_all().map_err(|e| format!("fsync failed: {e}"))
}

/// Delete a vault file. An already-missing file is success.
#[tauri::command]
pub fn db_delete_file(state: State<AppState>, rel_path: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    let _ = std::fs::remove_file(abs);
    Ok(())
}

// ---- v2.45: vault assets --------------------------------------------------

/// `readVaultAssetBase64` — an asset's bytes without handing the renderer a
/// host path. Upstream needs this because a remote workspace has no local file
/// to point at; here it is simply the honest way to read an asset the webview
/// cannot reach through the asset protocol.
#[tauri::command]
pub fn vault_read_asset_base64(state: State<AppState>, asset_path: String) -> Result<String, String> {
    use base64::Engine;
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &asset_path)?;
    let bytes = std::fs::read(&abs).map_err(|e| format!("read failed: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// `openAssetExternally` — open a VAULT asset with the OS default app. The
/// vault is always local in this build, so this is a containment check away
/// from `vault_open_external_file`; the `{ok, error}` shape (never a throw)
/// drives the renderer's toast the same way.
#[tauri::command]
pub fn vault_open_asset_externally(
    app: AppHandle,
    state: State<AppState>,
    rel_path: String,
) -> crate::ipc::types::OpenExternalFileResult {
    let result = require_root(&state)
        .and_then(|root| notes::resolve_safe(&root, &rel_path))
        .and_then(|abs| {
            if !abs.is_file() {
                return Err(format!("No such asset: {rel_path}"));
            }
            crate::os::open_external_file(&app, &abs.to_string_lossy())
        });
    match result {
        Ok(()) => crate::ipc::types::OpenExternalFileResult { ok: true, error: None },
        Err(error) => {
            crate::ipc::types::OpenExternalFileResult { ok: false, error: Some(error) }
        }
    }
}

// ---- v2.45: link-aware primitives for the workflow applier ----------------
// `db_write_text` is temp-file + rename, and a rename replaces the DIRECTORY
// ENTRY: pointed at a symlinked note it would leave a regular file where the
// link was and detach the link from its target for good. `write_note` in
// crud.rs writes through the link, so a workflow editing a note must too.
// Resolving the link and doing the atomic dance at the target keeps both
// properties: the link survives and no reader sees a half-written file.
//
// The target may sit outside the vault. That is what following a link means,
// and it is the same reach every other save in the app has — containment is
// enforced on the vault-relative path that named the link.

/// The file a note path really writes to: the symlink's target, or the path
/// itself. Returns the resolved absolute path.
fn through_links(abs: &std::path::Path) -> PathBuf {
    match std::fs::symlink_metadata(abs) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(abs).unwrap_or_else(|_| {
            // A dangling link still names where it points.
            match std::fs::read_link(abs) {
                Ok(target) if target.is_absolute() => target,
                Ok(target) => abs.parent().map(|p| p.join(target)).unwrap_or_else(|| abs.to_path_buf()),
                Err(_) => abs.to_path_buf(),
            }
        }),
        _ => abs.to_path_buf(),
    }
}

#[tauri::command]
pub fn db_write_through_links(
    state: State<AppState>,
    rel_path: String,
    text: String,
) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    db_atomic_write(&through_links(&abs), &text)
}

/// Remove what a note path names, following a symlink to its target: undo
/// deletes the file the run CREATED, and through a link that file is the
/// target, never the link. Then walk up removing directories the run emptied,
/// stopping before the vault's top-level folders — `inbox/2026` left by an
/// undone create is clutter, but `inbox` itself is furniture.
#[tauri::command]
pub fn db_remove_through_links(state: State<AppState>, rel_path: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    let target = through_links(&abs);
    let _ = std::fs::remove_file(&target);
    // Only prune inside the vault: a link's target can live anywhere.
    if target.starts_with(&root) {
        prune_empty_dirs(&root, target.parent());
    }
    Ok(())
}

fn prune_empty_dirs(root: &std::path::Path, start: Option<&std::path::Path>) {
    let Some(mut dir) = start.map(|d| d.to_path_buf()) else { return };
    while dir.starts_with(root) && dir != root {
        // Strictly below a top-level folder, so `inbox` survives.
        if dir.parent() == Some(root) {
            return;
        }
        // Best effort: a leftover empty directory is not a reason to fail.
        if std::fs::remove_dir(&dir).is_err() {
            return;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return,
        }
    }
}

// ---- v2.73: what the workflow applier needs to carry sidecars and links ----
// Upstream's applier (apps/desktop/src/main/workflow-apply.ts) moves each
// note's comments, creation date and agent edit record with it (#839), and
// puts a symlinked note back as the same link on undo. These are the file
// operations that takes, each as narrow as the applier's use of it.

/// The text of the symlink at a vault path, or None when it is not one.
#[tauri::command]
pub fn db_link_text(state: State<AppState>, rel_path: String) -> Result<Option<String>, String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    match std::fs::symlink_metadata(&abs) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::read_link(&abs)
            .map(|text| Some(text.to_string_lossy().to_string()))
            .map_err(|e| format!("readlink failed: {e}")),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("stat failed: {e}")),
    }
}

/// Whether anything at all sits at a vault path, a symlink that points at
/// nothing included (`db_exists` follows links and calls such a link free,
/// and a rename onto it would replace the link without a trace).
#[tauri::command]
pub fn db_path_taken(state: State<AppState>, rel_path: String) -> Result<bool, String> {
    let root = require_root(&state)?;
    sidecars::path_exists(&notes::resolve_safe(&root, &rel_path)?).map_err(|e| format!("stat failed: {e}"))
}

/// Give the symlink at a vault path the text `text`, which must name the same
/// file the link already names: a spelling, never a new target. Checked here
/// rather than trusted, so an edited or synced run ledger cannot use undo to
/// plant a link to anywhere and then write through it.
#[tauri::command]
pub fn db_respell_link(state: State<AppState>, rel_path: String, text: String) -> Result<(), String> {
    let root = require_root(&state)?;
    sidecars::respell_link(&notes::resolve_safe(&root, &rel_path)?, &text)
}

/// Rename a note (or a sidecar) the way the app moves one: parents created,
/// and a symlinked note keeps pointing at its file (`rebaseMovedLink`).
#[tauri::command]
pub fn db_rename_entry(state: State<AppState>, old_rel: String, new_rel: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let old_abs = notes::resolve_safe(&root, &old_rel)?;
    let new_abs = notes::resolve_safe(&root, &new_rel)?;
    if let Some(parent) = new_abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    std::fs::rename(&old_abs, &new_abs).map_err(|e| format!("rename failed: {e}"))?;
    sidecars::rebase_moved_link(&old_abs, &new_abs).map_err(|e| format!("relink failed: {e}"))
}

/// Remove what sits at a vault path AS ITSELF (a file, or a symlink, never
/// through it): the file a link points at is not the run's to delete. Then
/// prune the directories that leaves empty, below the top-level folders.
#[tauri::command]
pub fn db_remove_entry(state: State<AppState>, rel_path: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let abs = notes::resolve_safe(&root, &rel_path)?;
    match std::fs::remove_file(&abs) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("remove failed: {e}")),
        _ => {}
    }
    prune_empty_dirs(&root, abs.parent());
    Ok(())
}

/// The vault-relative path of a note's sidecar, derived from the note's path
/// every time (never taken from a ledger), so a restore can only ever aim at
/// some note's own comments, creation date or edit record.
#[tauri::command]
pub fn db_sidecar_rel(state: State<AppState>, note_rel: String, kind: String) -> Result<String, String> {
    let root = require_root(&state)?;
    let rel = sidecars::rel_of(&root, &notes::resolve_safe(&root, &note_rel)?);
    let abs = match kind.as_str() {
        "comments" => comments::comments_path(&root, &rel),
        "metadata" => sidecars::note_metadata_path(&root, &rel, false)?,
        "agent-edits" => sidecars::agent_edit_path(&root, &rel)?,
        other => return Err(format!("Unknown sidecar: {other}")),
    };
    Ok(sidecars::rel_of(&root, &abs))
}

/// Write a note's creation date down before its file is replaced or moved
/// (`prepareNoteCreation`). A temporary folder session gets no state.
#[tauri::command]
pub fn db_prepare_note_creation(state: State<AppState>, rel_path: String) -> Result<(), String> {
    let root = require_root(&state)?;
    if active_vault_is_temporary(&state) {
        return Ok(());
    }
    sidecars::prepare_note_creation(&root, &rel_path)
}

/// Prune empty directories upward from a sidecar's folder, strictly inside the
/// sidecar tree it belongs to. An empty leftover there is not harmless: a
/// folder rename refuses a destination whose comments or metadata directory
/// already exists, so one left behind by an undo would block that name.
#[tauri::command]
pub fn db_prune_sidecar_dirs(state: State<AppState>, rel_dir: String) -> Result<(), String> {
    let root = require_root(&state)?;
    let start = notes::resolve_safe(&root, &rel_dir)?;
    let bases = [
        PathBuf::from(config::resolve_path(&comments::comments_root(&root).to_string_lossy())),
        sidecars::note_metadata_root(&root),
        sidecars::agent_edits_root(&root),
    ];
    let Some(base) = bases.into_iter().find(|b| start != *b && start.starts_with(b)) else {
        return Ok(());
    };
    let mut dir = start;
    while dir != base && dir.starts_with(&base) {
        let empty = std::fs::read_dir(&dir).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !empty || std::fs::remove_dir(&dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    Ok(())
}
