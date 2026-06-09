//! Remote-workspace commands (M14, partial). Profile CRUD is implemented and
//! persisted; the live server connection is deferred (see remote.rs).

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::ipc::types::{RemoteWorkspaceProfile, RemoteWorkspaceProfileInput};
use crate::remote;

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))
}

const UNSUPPORTED: &str =
    "Remote workspaces require the SynNotes server client, which is not in this build yet.";

#[tauri::command]
pub fn workspace_list_remote_profiles(app: AppHandle) -> Result<Vec<RemoteWorkspaceProfile>, String> {
    Ok(remote::list_profiles(&config_dir(&app)?))
}

#[tauri::command]
pub fn workspace_save_remote_profile(
    app: AppHandle,
    input: RemoteWorkspaceProfileInput,
) -> Result<RemoteWorkspaceProfile, String> {
    remote::save_profile(&config_dir(&app)?, &input)
}

#[tauri::command]
pub fn workspace_delete_remote_profile(app: AppHandle, id: String) -> Result<(), String> {
    remote::delete_profile(&config_dir(&app)?, &id)
}

#[tauri::command]
pub fn workspace_connect_remote() -> Result<serde_json::Value, String> {
    Err(UNSUPPORTED.into())
}

#[tauri::command]
pub fn workspace_connect_remote_profile() -> Result<serde_json::Value, String> {
    Err(UNSUPPORTED.into())
}

#[tauri::command]
pub fn workspace_browse_server_directories() -> Result<serde_json::Value, String> {
    Err(UNSUPPORTED.into())
}
