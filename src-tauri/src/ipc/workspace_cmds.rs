//! Remote-workspace commands: upstream's `workspace:*` IPC handlers plus the
//! transport the webview's server client (src/bridge/remote/client.ts) runs on.
//!
//! Everything that touches the network is async and runs on the blocking
//! pool: a sync Tauri command runs on the main thread, and a slow server must
//! never freeze the window.

use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::ipc::types::{OpenExternalFileResult, RemoteWorkspaceProfile, RemoteWorkspaceProfileInput, VaultInfo};
use crate::remote::http::Method;
use crate::remote::{profiles, session, RemoteError};
use crate::state::AppState;

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))
}

async fn blocking<T: Send + 'static, E: From<String> + Send + 'static>(
    f: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, E> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| E::from(e.to_string()))?
}

/// Connects, boots and disconnects take turns.
fn with_workspace_lock<T>(app: &AppHandle, f: impl FnOnce() -> T) -> T {
    let state = app.state::<AppState>();
    let _workspace = state.workspace_lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

// ---- Saved profiles --------------------------------------------------------

#[tauri::command]
pub fn workspace_list_remote_profiles(app: AppHandle) -> Result<Vec<RemoteWorkspaceProfile>, String> {
    Ok(profiles::list_profiles(&config_dir(&app)?))
}

#[tauri::command]
pub async fn workspace_save_remote_profile(
    app: AppHandle,
    input: RemoteWorkspaceProfileInput,
) -> Result<RemoteWorkspaceProfile, String> {
    // The keychain can block (a locked keyring asking to unlock).
    blocking(move || {
        let dir = config_dir(&app)?;
        let live = session::live_connection(&app.state::<AppState>());
        let live = live.as_ref().map(|(base_url, root)| profiles::LiveConnection {
            base_url,
            vault_root: root.as_deref(),
        });
        let saved = profiles::save_profile(&dir, &input, live)?;
        // The live connection is the profile the config now calls current.
        if let Some(session) = app.state::<AppState>().remote_session() {
            let current = profiles::current_profile_id(&crate::vault::config::load_config(&dir));
            session.set_profile_id(current);
        }
        Ok(saved)
    })
    .await
}

#[tauri::command]
pub async fn workspace_delete_remote_profile(
    app: AppHandle,
    window: tauri::Window,
    id: String,
) -> Result<(), String> {
    let origin = window.label().to_string();
    blocking(move || {
        let live = app.state::<AppState>().remote_session();
        let deleted = profiles::delete_profile(&config_dir(&app)?, &id)?;
        if let Some(live) = live.filter(|s| s.profile_id().as_deref() == Some(id.as_str())) {
            if deleted.dropped_live_credential {
                session::drop_session(&app, Some(&origin));
            } else {
                live.set_profile_id(None);
            }
        }
        Ok(())
    })
    .await
}

// ---- The connection --------------------------------------------------------

#[tauri::command]
pub fn workspace_get_info(app: AppHandle) -> Option<Value> {
    session::info(&app)
}

#[tauri::command]
pub async fn workspace_connect_remote(
    app: AppHandle,
    window: tauri::Window,
    base_url: String,
    auth_token: Option<String>,
) -> Result<Value, RemoteError> {
    let origin = window.label().to_string();
    blocking(move || {
        with_workspace_lock(&app, || {
            session::connect(
                &app,
                &base_url,
                auth_token.as_deref(),
                session::ConnectOptions { origin: Some(origin), persist: true, profile_id: None, vault_path: None },
            )
        })
    })
    .await
}

#[tauri::command]
pub async fn workspace_connect_remote_profile(
    app: AppHandle,
    window: tauri::Window,
    id: String,
) -> Result<Value, RemoteError> {
    let origin = window.label().to_string();
    blocking(move || with_workspace_lock(&app, || session::connect_profile(&app, Some(&origin), &id))).await
}

#[tauri::command]
pub async fn workspace_disconnect_remote(app: AppHandle, window: tauri::Window) -> Result<Option<VaultInfo>, String> {
    let origin = window.label().to_string();
    blocking(move || with_workspace_lock(&app, || session::disconnect(&app, Some(&origin)))).await
}

/// `workspace:retry-boot`: the same path as a launch.
#[tauri::command]
pub async fn workspace_retry_boot(app: AppHandle, window: tauri::Window) -> Result<Option<Value>, String> {
    let origin = window.label().to_string();
    blocking(move || crate::ipc::vault_cmds::load_current_vault(&app, Some(&origin))).await
}

#[tauri::command]
pub async fn workspace_browse_server_directories(
    app: AppHandle,
    path: Option<String>,
) -> Result<Value, RemoteError> {
    blocking(move || session::browse(&app, path.as_deref().unwrap_or(""))).await
}

// ---- The transport -----------------------------------------------------------

/// One request from the webview's server client. The path is checked against
/// the protocol's route shape and always goes to the connected server, with
/// the session's token.
#[tauri::command]
pub async fn remote_request(
    app: AppHandle,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, RemoteError> {
    blocking(move || {
        let method = Method::parse(&method)?;
        let session = session::session(&app)?;
        session.transport.json(method, &path, body.as_ref())
    })
    .await
}

/// The client found its list behind the server (a 404 for a path it named):
/// every window re-pulls, as upstream's `onStalePath` does.
#[tauri::command]
pub fn remote_resync(app: AppHandle) {
    session::send_resync(&app);
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "attachment".to_string())
}

/// `VAULT_IMPORT_FILES` on a remote workspace: the dropped files are on this
/// machine and the vault is not, so each one is streamed to the server's
/// upload route. Failures are collected per file, so the files that did
/// upload still get their markdown.
#[tauri::command]
pub async fn remote_upload_files(
    app: AppHandle,
    note_path: String,
    source_paths: Vec<String>,
) -> Result<Vec<Value>, RemoteError> {
    blocking(move || {
        let session = session::session(&app)?;
        let mut imported = Vec::new();
        let mut failures = Vec::new();
        for source in source_paths {
            let abs = PathBuf::from(crate::vault::config::resolve_path(&source));
            let name = file_name_of(&abs);
            let uploaded = std::fs::metadata(&abs)
                .map_err(|e| e.to_string())
                .and_then(|meta| {
                    if !meta.is_file() {
                        return Ok(None);
                    }
                    let file = std::fs::File::open(&abs).map_err(|e| e.to_string())?;
                    session
                        .transport
                        .upload(&note_path, &name, "application/octet-stream", file, meta.len())
                        .map(Some)
                        .map_err(|e| e.message)
                });
            match uploaded {
                Ok(Some(asset)) => imported.push(asset),
                Ok(None) => {}
                Err(message) => failures.push(format!("{name}: {message}")),
            }
        }
        if imported.is_empty() && !failures.is_empty() {
            return Err(RemoteError::other(format!("Import failed. {}", failures.join("; "))));
        }
        if !failures.is_empty() {
            eprintln!("[remote] some dropped files failed to import: {}", failures.join("; "));
        }
        Ok(imported)
    })
    .await
}

/// `uploadAsset` from bytes the webview holds (a pasted image).
#[tauri::command]
pub async fn remote_upload_bytes(
    app: AppHandle,
    note_path: String,
    file_name: String,
    mime_type: String,
    data: Vec<u8>,
) -> Result<Value, RemoteError> {
    blocking(move || {
        let session = session::session(&app)?;
        let len = data.len() as u64;
        let mime = if mime_type.trim().is_empty() { "application/octet-stream" } else { mime_type.trim() };
        session.transport.upload(&note_path, &file_name, mime, &data[..], len)
    })
    .await
}

/// Assets read whole into memory stop at this size.
const MAX_ASSET_IN_MEMORY: u64 = 512 * 1024 * 1024;

/// `VAULT_READ_ASSET_BASE64` on a remote workspace.
#[tauri::command]
pub async fn remote_read_asset_base64(app: AppHandle, asset_path: String) -> Result<String, RemoteError> {
    blocking(move || {
        let rel = asset_path.trim();
        if rel.is_empty() {
            return Err(RemoteError::other("Asset path is required."));
        }
        let bytes = session::session(&app)?.transport.asset_bytes(rel, MAX_ASSET_IN_MEMORY)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    })
    .await
}

/// The local file name for a downloaded asset: the last component of a path a
/// server named, on any platform. Both separators split it, characters Windows
/// reserves (a drive prefix's `:` among them) become `_`, and what is left must
/// be one plain path component, or it is `attachment`.
fn download_file_name(rel: &str) -> String {
    let last = rel.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    let mut components = Path::new(&cleaned).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => cleaned,
        _ => "attachment".to_string(),
    }
}

/// Download a remote asset into a fresh temp folder of its own (a second open
/// must not overwrite a file the first app still has open) and hand it to the
/// OS. The OS owns temp cleanup.
pub(crate) fn open_remote_asset(app: &AppHandle, rel: &str) -> Result<(), String> {
    let session = session::session(app).map_err(|e| e.message)?;
    let response = session.transport.asset(rel, None).map_err(|e| e.message)?;
    let dir = std::env::temp_dir().join(format!("zennotes-rs-remote-open-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(download_file_name(rel));
    if target.parent() != Some(dir.as_path()) {
        return Err("The server named an attachment this app cannot save.".to_string());
    }
    let mut file = std::fs::File::create(&target).map_err(|e| e.to_string())?;
    std::io::copy(&mut response.into_reader().take(MAX_ASSET_IN_MEMORY * 8), &mut file)
        .map_err(|e| e.to_string())?;
    drop(file);
    crate::os::open_external_file(app, &target.to_string_lossy())
}

/// `VAULT_OPEN_ASSET_EXTERNALLY` on a remote workspace: never a throw, the
/// `{ok, error}` shape drives the renderer's toast.
#[tauri::command]
pub async fn remote_open_asset_externally(app: AppHandle, rel_path: String) -> OpenExternalFileResult {
    let result = blocking(move || {
        let rel = rel_path.trim().to_string();
        if rel.is_empty() {
            return Err("Empty path.".to_string());
        }
        open_remote_asset(&app, &rel)
    })
    .await;
    match result {
        Ok(()) => OpenExternalFileResult { ok: true, error: None },
        Err(error) => OpenExternalFileResult { ok: false, error: Some(error) },
    }
}

#[cfg(test)]
mod tests {
    use super::download_file_name;

    #[test]
    fn a_downloaded_name_is_one_plain_component_on_every_platform() {
        assert_eq!(download_file_name("assets/photo 1.png"), "photo 1.png");
        assert_eq!(download_file_name("assets/../../etc/passwd"), "passwd");
        assert_eq!(download_file_name("assets/..\\..\\evil.exe"), "evil.exe");
        assert_eq!(download_file_name("assets/C:evil.exe"), "C_evil.exe");
        assert_eq!(download_file_name("C:\\Windows\\x.dll"), "x.dll");
        assert_eq!(download_file_name("assets/a<b>|c?.png"), "a_b__c_.png");
        assert_eq!(download_file_name("assets/.."), "attachment");
        assert_eq!(download_file_name("assets/..."), "attachment");
        assert_eq!(download_file_name("assets/"), "attachment");
        assert_eq!(download_file_name(""), "attachment");
        assert_eq!(download_file_name("assets/name. "), "name");
    }
}
