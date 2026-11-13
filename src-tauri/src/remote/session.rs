//! The live connection to a ZenNotes server: port of upstream's
//! `setRemoteWorkspace`, `disconnectRemoteWorkspace`,
//! `connectRemoteWorkspaceProfile`, `loadCurrentVaultFromConfig` (its remote
//! half), `currentRemoteWorkspaceInfo` and the `VAULT_SELECT_PATH` handler
//! (apps/desktop/src/main/index.ts).
//!
//! Remote mode is app-wide here: every window already follows the one open
//! vault (2.79.1), so there is one session, not upstream's per-window registry.
//! While it is on, no local vault is open, so every local vault command finds
//! no root and refuses. That is the guard against a call that missed routing
//! resolving a server path such as `/My vaults/Research` on this disk.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::http::{Method, RemoteError, Transport};
use super::profiles;
use super::watch::{self, FeedEvent, FeedHandle, FeedTimings};
use crate::ipc::types::VaultInfo;
use crate::secrets;
use crate::state::AppState;
use crate::vault::config::{self, PersistedConfig};

/// Every window re-reads `getRemoteWorkspaceInfo` on this.
pub const WORKSPACE_CHANGED_EVENT: &str = "workspace://changed";
/// The local watcher's event: remote changes take the same channel, as upstream.
pub const VAULT_CHANGE_EVENT: &str = "vault://change";

pub struct RemoteSession {
    pub transport: Transport,
    /// The saved profile this connection is, when it is one. Saving a profile
    /// that describes the connection makes it this one (upstream's
    /// `currentRemoteWorkspaceProfileId`), so it changes during a session.
    profile_id: Mutex<Option<String>>,
    capabilities: Mutex<Value>,
    vault: Mutex<Value>,
    feed: Mutex<Option<FeedHandle>>,
}

impl RemoteSession {
    pub fn profile_id(&self) -> Option<String> {
        self.profile_id.lock().unwrap().clone()
    }

    pub fn set_profile_id(&self, id: Option<String>) {
        *self.profile_id.lock().unwrap() = id;
    }

    pub fn capabilities(&self) -> Value {
        self.capabilities.lock().unwrap().clone()
    }

    fn set_capabilities(&self, caps: Value) {
        *self.capabilities.lock().unwrap() = caps;
    }

    /// The server's `VaultInfo`, passed through untouched, or `null`.
    pub fn vault(&self) -> Value {
        self.vault.lock().unwrap().clone()
    }

    fn set_vault(&self, vault: Value) {
        *self.vault.lock().unwrap() = vault;
    }

    pub fn vault_root(&self) -> Option<String> {
        self.vault().get("root").and_then(Value::as_str).map(str::to_string)
    }

    pub fn vault_info(&self) -> Option<VaultInfo> {
        serde_json::from_value(self.vault()).ok()
    }

    fn capability(&self, name: &str) -> bool {
        self.capabilities().get(name).and_then(Value::as_bool) == Some(true)
    }

    fn stop_feed(&self) {
        self.feed.lock().unwrap().take();
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        self.stop_feed();
    }
}

fn config_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve app config dir: {e}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The workspace switched (connected, disconnected, another vault selected,
/// a local vault took over). `origin` is the window that asked: it carries on
/// with its own store, and every other window reloads onto the new workspace,
/// since all of them follow the one open vault.
pub fn notify_workspace_changed(app: &AppHandle, origin: Option<&str>) {
    let _ = app.emit(WORKSPACE_CHANGED_EVENT, json!({ "origin": origin }));
}

/// `sendRemoteResync`: tell every window its list is behind the server.
pub fn send_resync(app: &AppHandle) {
    let _ = app.emit(
        VAULT_CHANGE_EVENT,
        json!({ "kind": "change", "path": "", "folder": "inbox", "scope": "resync" }),
    );
}

/// `startRemoteWatch`.
fn start_feed(app: &AppHandle, session: &RemoteSession) {
    session.stop_feed();
    if !session.capability("supportsWatch") {
        // A server can run with its watcher off; say so once, or "no live
        // updates" leaves no trace anywhere.
        eprintln!(
            "[remote] {} reports no watch support; live updates are off for this workspace",
            session.transport.base_url()
        );
        return;
    }
    let sink_app = app.clone();
    let started = watch::spawn(
        session.transport.base_url(),
        session.transport.token(),
        FeedTimings::default(),
        move |event| match event {
            FeedEvent::Change(change) => {
                let _ = sink_app.emit(VAULT_CHANGE_EVENT, change);
            }
            FeedEvent::Resync => send_resync(&sink_app),
        },
    );
    match started {
        Ok(handle) => *session.feed.lock().unwrap() = Some(handle),
        Err(err) => eprintln!("[remote] the change feed could not start: {err}"),
    }
}

pub struct ConnectOptions {
    /// The window that asked.
    pub origin: Option<String>,
    pub persist: bool,
    pub profile_id: Option<String>,
    pub vault_path: Option<String>,
}

fn require_session(state: &AppState) -> Result<Arc<RemoteSession>, RemoteError> {
    state
        .remote_session()
        .ok_or_else(|| RemoteError::other("No remote workspace is connected"))
}

/// `setRemoteWorkspace`: ask the server who it is and which vault this
/// credential is on, switch to the profile's vault when it names another one,
/// then make the session the app's workspace.
pub fn connect(
    app: &AppHandle,
    base_url: &str,
    token: Option<&str>,
    options: ConnectOptions,
) -> Result<Value, RemoteError> {
    let state = app.state::<AppState>();
    let generation = state.workspace_generation();
    let transport = Transport::new(base_url, token)?;
    let mut capabilities = transport.json(Method::Get, "/api/capabilities", None)?;
    state.set_remote_boot_error(None);
    let mut vault = transport.json(Method::Get, "/api/vault", None)?;
    let preferred = options.vault_path.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let supports_selection = capabilities.get("supportsVaultSelection").and_then(Value::as_bool) == Some(true);
    if let Some(preferred) = preferred {
        let current_root = vault.get("root").and_then(Value::as_str);
        if supports_selection && current_root != Some(preferred) {
            vault = transport.json(Method::Post, "/api/vault/select", Some(&json!({ "path": preferred })))?;
            // supportsWatch is per watcher on the server: ask again for the
            // vault this session actually landed on.
            capabilities = transport.json(Method::Get, "/api/capabilities", None)?;
        }
    }

    // Something else (a local vault opened meanwhile) owns the workspace now.
    if state.workspace_generation() != generation {
        return Err(RemoteError::other("Another vault was opened while connecting."));
    }

    let session = Arc::new(RemoteSession {
        transport,
        profile_id: Mutex::new(options.profile_id.clone()),
        capabilities: Mutex::new(capabilities.clone()),
        vault: Mutex::new(vault.clone()),
        feed: Mutex::new(None),
    });
    // The local vault closes: its watcher stops and its root is gone from
    // the state, so local commands refuse while the session is on.
    state.set_watcher(None);
    state.set_current(None);
    start_feed(app, &session);
    let base = session.transport.base_url().to_string();
    state.set_remote_session(Some(session));

    if options.persist {
        let dir = config_dir(app)?;
        let profile_id = options.profile_id.clone();
        config::update_config(&dir, move |cfg| {
            cfg.workspace_mode = "remote".into();
            profiles::set_connection_base_url(cfg, Some(&base));
            profiles::set_current_profile_id(cfg, profile_id.as_deref());
        })
        .map_err(|e| RemoteError::other(format!("Failed to persist config: {e}")))?;
    }
    notify_workspace_changed(app, options.origin.as_deref());
    Ok(json!({ "vault": vault, "capabilities": capabilities }))
}

/// A connection without a saved token that the server turns away is missing a
/// token, not holding a wrong one (#870). Here, without a keychain, a token
/// typed in an earlier run was never kept, which deserves saying.
fn missing_token_message(name: &str, base_url: &str) -> String {
    if secrets::remote_tokens_persist() {
        format!(
            "No auth token is saved for \"{name}\", and {base_url} needs one. Choose Edit on it under Settings → Vault → Saved Remote Workspaces and add the server's token."
        )
    } else {
        format!(
            "No auth token is saved for \"{name}\", and {base_url} needs one. This system has no keychain, so ZenNotes-rs keeps a token only until it quits. Choose Edit on it under Settings → Vault → Saved Remote Workspaces and add the server's token again."
        )
    }
}

/// `connectRemoteWorkspaceProfile`.
pub fn connect_profile(app: &AppHandle, origin: Option<&str>, id: &str) -> Result<Value, RemoteError> {
    let dir = config_dir(app)?;
    let cfg = config::load_config(&dir);
    let profile = profiles::find_profile(&cfg, Some(id))
        .ok_or_else(|| RemoteError::other("That saved remote workspace no longer exists."))?;
    let token = secrets::get_secret(&dir, &profile.id);
    let result = connect(
        app,
        &profile.base_url,
        token.as_deref(),
        ConnectOptions {
            origin: origin.map(str::to_string),
            persist: true,
            profile_id: Some(profile.id.clone()),
            vault_path: profile.vault_path.clone(),
        },
    );
    let value = match result {
        Err(err) if token.is_none() && err.status == Some(401) => {
            return Err(RemoteError::request(401, missing_token_message(&profile.name, &profile.base_url)));
        }
        other => other?,
    };
    profiles::mark_connected(&dir, &profile.id, now_ms())?;
    Ok(value)
}

/// The remote half of `loadCurrentVaultFromConfig`: reconnect the configured
/// workspace without re-saving it. A failure keeps the workspace configured
/// and records why, so the renderer shows a reconnect screen instead of the
/// first-run Welcome.
pub fn boot(app: &AppHandle, cfg: &PersistedConfig, origin: Option<&str>) -> Option<Value> {
    let base_url = profiles::connection_base_url(cfg)?;
    let dir = config_dir(app).ok()?;
    let profile_id = profiles::current_profile_id(cfg);
    let profile = profiles::find_profile(cfg, profile_id.as_deref());
    let token = profile.as_ref().and_then(|p| secrets::get_secret(&dir, &p.id));
    let result = connect(
        app,
        &base_url,
        token.as_deref(),
        ConnectOptions {
            origin: origin.map(str::to_string),
            persist: false,
            profile_id: profile.as_ref().map(|p| p.id.clone()).or(profile_id),
            vault_path: profile.as_ref().and_then(|p| p.vault_path.clone()),
        },
    );
    match result {
        Ok(value) => {
            app.state::<AppState>().set_remote_boot_error(None);
            let vault = value.get("vault").cloned().unwrap_or(Value::Null);
            (!vault.is_null()).then_some(vault)
        }
        Err(err) => {
            let message = match (&profile, token.is_none(), err.status) {
                (Some(p), true, Some(401)) => missing_token_message(&p.name, &p.base_url),
                _ if err.message.trim().is_empty() => {
                    format!("Could not connect to the ZenNotes server at {base_url}.")
                }
                _ => err.message,
            };
            // No event: the window that asked reads the error itself, and a
            // reload elsewhere would only boot, fail and reload again.
            app.state::<AppState>().set_remote_boot_error(Some(message));
            None
        }
    }
}

/// `disconnectRemoteWorkspace`: back to the last local vault, or to none.
pub fn disconnect(app: &AppHandle, origin: Option<&str>) -> Result<Option<VaultInfo>, String> {
    let state = app.state::<AppState>();
    state.set_remote_session(None);
    state.set_remote_boot_error(None);
    let dir = config_dir(app)?;
    let cfg = config::load_config(&dir);
    let vault = match cfg.vault_root {
        Some(root) => Some(crate::ipc::vault_cmds::open_local_vault_root(app, &state, &root, origin)?),
        None => {
            state.set_current(None);
            state.set_watcher(None);
            None
        }
    };
    config::update_config(&dir, |cfg| {
        cfg.workspace_mode = "local".into();
        profiles::set_current_profile_id(cfg, None);
    })
    .map_err(|e| format!("Failed to persist config: {e}"))?;
    notify_workspace_changed(app, origin);
    Ok(vault)
}

/// `VAULT_SELECT_PATH`: move this credential to another vault on the server.
pub fn select_vault_path(app: &AppHandle, origin: Option<&str>, path: &str) -> Result<Value, RemoteError> {
    let session = require_session(&app.state::<AppState>())?;
    let vault = session
        .transport
        .json(Method::Post, "/api/vault/select", Some(&json!({ "path": path })))?;
    session.set_vault(vault.clone());
    // supportsWatch is per watcher on the server, so the new root may watch
    // where the old one did not, or the reverse. Keep the old answer on error.
    if let Ok(caps) = session.transport.json(Method::Get, "/api/capabilities", None) {
        session.set_capabilities(caps);
    }
    start_feed(app, &session);
    notify_workspace_changed(app, origin);
    Ok(vault)
}

/// `VAULT_BROWSE_SERVER_DIRECTORIES`.
pub fn browse(app: &AppHandle, path: &str) -> Result<Value, RemoteError> {
    let session = require_session(&app.state::<AppState>())?;
    let query = if path.is_empty() {
        String::new()
    } else {
        format!("?path={}", urlencoding::encode(path))
    };
    session.transport.json(Method::Get, &format!("/api/fs/browse{query}"), None)
}

/// `currentRemoteWorkspaceInfo`. Never carries the token, only whether there is
/// one.
pub fn info(app: &AppHandle) -> Option<Value> {
    let state = app.state::<AppState>();
    if let Some(session) = state.remote_session() {
        return Some(json!({
            "mode": "remote",
            "baseUrl": session.transport.base_url(),
            "authConfigured": session.transport.has_token(),
            "capabilities": session.capabilities(),
            "profileId": session.profile_id(),
            "bootError": Value::Null,
        }));
    }
    if state.current().is_some() {
        return None;
    }
    let cfg = config::load_config(&config_dir(app).ok()?);
    if cfg.workspace_mode != "remote" {
        return None;
    }
    let base_url = profiles::connection_base_url(&cfg)?;
    Some(json!({
        "mode": "remote",
        "baseUrl": base_url,
        "authConfigured": false,
        "capabilities": Value::Null,
        "profileId": profiles::current_profile_id(&cfg),
        "bootError": state.remote_boot_error(),
    }))
}

/// The live connection, for matching a saved profile against it.
pub fn live_connection(state: &AppState) -> Option<(String, Option<String>)> {
    let session = state.remote_session()?;
    Some((session.transport.base_url().to_string(), session.vault_root()))
}

/// The session a request runs on, or upstream's error when there is none.
pub fn session(app: &AppHandle) -> Result<Arc<RemoteSession>, RemoteError> {
    require_session(&app.state::<AppState>())
}

#[cfg(test)]
impl RemoteSession {
    /// A session on `base_url` with no feed, for state tests.
    pub fn for_test(base_url: &str, vault: Value) -> Arc<RemoteSession> {
        Arc::new(RemoteSession {
            transport: Transport::new(base_url, Some("t")).unwrap(),
            profile_id: Mutex::new(Some("p1".into())),
            capabilities: Mutex::new(json!({ "supportsWatch": false })),
            vault: Mutex::new(vault),
            feed: Mutex::new(None),
        })
    }
}

/// Saved-profile deletion that took the live connection's credential: the
/// session ends, as upstream drops its client.
pub fn drop_session(app: &AppHandle, origin: Option<&str>) {
    app.state::<AppState>().set_remote_session(None);
    notify_workspace_changed(app, origin);
}
