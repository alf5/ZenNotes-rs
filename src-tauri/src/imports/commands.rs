//! The import commands. Each is one of upstream's IPC channels
//! (`OBSIDIAN_IMPORT_*`, `LOGSEQ_IMPORT_*`, `NOTION_IMPORT_*`) or one of the
//! `fs` calls its run code makes; the webview side is src/bridge/imports/,
//! which names every command and argument here (camelCase there).
//!
//! `page` is the calling page's id (src/bridge/imports/host.ts): a run
//! belongs to the page that started it, and a reload ends it (session.rs).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use super::files::{self, EntryInfo, StatInfo};
use super::notion_auth;
use super::notion_http::{self, DownloadReply, HostReply};
use super::paths::{self, Place, Scope};
use super::session::{self, Caller, Kind, Run};
use crate::state::AppState;

const NO_RUN: &str = "No import is running.";
/// Upstream's refusal in every import handler while a remote workspace is on.
const LOCAL_ONLY: &str = "Imports write to a local vault.";

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

fn emit_progress(app: &AppHandle, kind: Kind, progress: Option<Value>) {
    if let Some(progress) = progress {
        let _ = app.emit(kind.progress_event(), progress);
    }
}

/// Ends a run whose page is gone, telling every window (session.rs).
fn reap(app: &AppHandle, kind: Kind, label: &str, page: &str) {
    let told = session::with_session(kind, |s| {
        session::reap_dead_run(s, &Caller { label, page }, |owner| app.get_webview_window(owner).is_some())
    });
    emit_progress(app, kind, told);
}

/// Upstream's `path.join(app.getPath('userData'), '<kind>-import', id)`,
/// one per vault so two vaults' half-finished imports can't mix.
fn staging_for(app: &AppHandle, kind: Kind, root: &Path) -> Result<PathBuf, String> {
    let data = app.path().app_data_dir().map_err(|e| format!("Could not resolve app data dir: {e}"))?;
    let resolved = crate::vault::config::resolve_path(&root.to_string_lossy());
    let id = format!("{:x}", Sha256::digest(resolved.as_bytes()));
    Ok(data.join(kind.staging_dir_name()).join(&id[..24]))
}

fn local_root(state: &AppState) -> Result<PathBuf, String> {
    if state.remote_session().is_some() {
        return Err(LOCAL_ONLY.to_string());
    }
    state.current_root().ok_or_else(|| "No vault is currently open".to_string())
}

// ---- runs -------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStatus {
    running: bool,
    progress: Option<Value>,
    /// Stop was pressed and the run hasn't ended yet. The page running it
    /// checks this once it listens for Stop, so an early press isn't lost.
    stopping: bool,
}

/// `OBSIDIAN_IMPORT_STATUS` / `LOGSEQ_IMPORT_STATUS`.
#[tauri::command]
pub async fn import_status(app: AppHandle, window: tauri::Window, kind: String, page: String) -> Result<RunStatus, String> {
    let kind = Kind::parse(&kind)?;
    reap(&app, kind, window.label(), &page);
    Ok(session::with_session(kind, |s| RunStatus {
        running: s.run.is_some(),
        progress: s.progress.clone(),
        stopping: s.run.as_ref().is_some_and(|run| run.stop.load(Ordering::SeqCst)),
    }))
}

/// `*_IMPORT_PICK`, the dialog half: the folder is kept here and only its
/// path comes back, for the person to see. Null when they cancel.
#[tauri::command]
pub async fn import_pick(
    app: AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    kind: String,
) -> Result<Option<String>, String> {
    let kind = Kind::parse(&kind)?;
    let title = match kind {
        Kind::Obsidian => "Choose your Obsidian vault",
        Kind::Logseq => "Choose your Logseq graph",
        Kind::Notion => return Err("Notion has no folder to pick.".to_string()),
    };
    let picked = app.dialog().file().set_title(title).set_parent(&window).blocking_pick_folder();
    let Some(picked) = picked else { return Ok(None) };
    let picked = picked.into_path().map_err(|e| format!("Invalid folder selection: {e}"))?;
    // The window's vault, or none while a remote workspace is on.
    let root = if state.remote_session().is_some() { None } else { state.current_root() };
    if let Some(root) = root {
        if paths::is_inside(&picked, &root) || paths::is_inside(&root, &picked) {
            return Err(kind.overlap_message().to_string());
        }
    }
    session::with_session(kind, |s| s.picked = Some(picked.clone()));
    Ok(Some(picked.to_string_lossy().into_owned()))
}

/// The picked folder becomes "the vault that was picked" (upstream's
/// `source = picked; lastProgress = null`).
#[tauri::command]
pub async fn import_adopt(kind: String) -> Result<(), String> {
    let kind = Kind::parse(&kind)?;
    session::with_session(kind, |s| {
        s.source = s.picked.clone();
        s.progress = None;
    });
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginInfo {
    root: String,
    source: String,
    staging_dir: String,
    notes_root: String,
    existing_notes: Vec<String>,
    existing_assets: Vec<String>,
}

/// The checks at the top of upstream's `*_IMPORT_START` handler and
/// `start()`, and what `start()` gathers: the run takes the open vault for
/// its whole life, its staging folder, its notes folder and what the vault
/// already holds. Paths come back virtual.
#[tauri::command]
pub async fn import_begin(
    app: AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    kind: String,
    page: String,
) -> Result<BeginInfo, String> {
    let kind = Kind::parse(&kind)?;
    reap(&app, kind, window.label(), &page);
    if session::with_session(kind, |s| s.run.is_some()) {
        return Err("An import is already running.".to_string());
    }
    let root = local_root(&state)?;
    let source = match kind {
        Kind::Notion => None,
        _ => {
            let source = session::with_session(kind, |s| s.source.clone())
                .ok_or_else(|| kind.choose_first_message().to_string())?;
            if paths::is_inside(&source, &root) || paths::is_inside(&root, &source) {
                return Err(kind.overlap_message().to_string());
            }
            Some(source)
        }
    };
    let staging = staging_for(&app, kind, &root)?;
    let listed = root.clone();
    let (notes_rel, existing_notes, existing_assets) = blocking(move || {
        let notes_root = crate::vault::layout::folder_root(&listed, "inbox");
        let rel = crate::vault::sidecars::rel_of(&listed, &notes_root);
        let notes = match kind {
            Kind::Notion => Vec::new(),
            _ => crate::vault::listing::list_note_paths(&listed).into_iter().map(|(path, _)| path).collect(),
        };
        let assets = match kind {
            Kind::Obsidian => crate::vault::assets::list_assets(&listed).into_iter().map(|a| a.path).collect(),
            _ => Vec::new(),
        };
        Ok((rel, notes, assets))
    })
    .await?;
    let owner_label = window.label().to_string();
    session::with_session(kind, |s| {
        if s.run.is_some() {
            return Err("An import is already running.".to_string());
        }
        s.run = Some(Run {
            root,
            staging,
            source,
            owner_label,
            owner_page: page,
            moved_in: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
        });
        s.progress = None;
        Ok(())
    })?;
    let top = format!("/{}", kind.name());
    let notes_root = if notes_rel.is_empty() { format!("{top}/vault") } else { format!("{top}/vault/{notes_rel}") };
    Ok(BeginInfo {
        root: format!("{top}/vault"),
        source: format!("{top}/source"),
        staging_dir: format!("{top}/staging"),
        notes_root,
        existing_notes,
        existing_assets,
    })
}

/// A run that began but couldn't start (upstream's `catch` in `start()`).
#[tauri::command]
pub async fn import_end(kind: String, page: String) -> Result<(), String> {
    let kind = Kind::parse(&kind)?;
    session::with_session(kind, |s| {
        if s.run.as_ref().is_some_and(|run| run.owner_page == page) {
            s.run = None;
        }
    });
    Ok(())
}

/// Upstream's `broadcast`: kept for status, and sent to every window on
/// `*_IMPORT_ON_PROGRESS`. The webview spaces them out as upstream does.
#[tauri::command]
pub async fn import_report(app: AppHandle, kind: String, page: String, progress: Value) -> Result<(), String> {
    let kind = Kind::parse(&kind)?;
    if session::with_session(kind, |s| session::record_progress(kind, s, &page, progress.clone())) {
        let _ = app.emit(kind.progress_event(), progress);
    }
    Ok(())
}

/// `*_IMPORT_STOP`: the page running the import hears it and aborts.
#[tauri::command]
pub async fn import_stop(app: AppHandle, window: tauri::Window, kind: String, page: String) -> Result<(), String> {
    let kind = Kind::parse(&kind)?;
    reap(&app, kind, window.label(), &page);
    let stopping = session::with_session(kind, |s| {
        s.run.as_ref().map(|run| run.stop.store(true, Ordering::SeqCst)).is_some()
    });
    if stopping {
        let _ = app.emit(session::STOP_EVENT, kind.name());
    }
    Ok(())
}

/// `*_IMPORT_DISMISS`: a finished result has been seen.
#[tauri::command]
pub async fn import_dismiss(kind: String) -> Result<(), String> {
    let kind = Kind::parse(&kind)?;
    session::with_session(kind, |s| {
        if s.run.is_none() {
            s.progress = None;
        }
    });
    Ok(())
}

// ---- files ------------------------------------------------------------------

fn place(text: &str) -> Result<Place, String> {
    let vpath = paths::parse(text)?;
    let base = session::with_session(vpath.kind, |s| match vpath.scope {
        Scope::Picked => s.picked.clone().ok_or_else(|| "Choose a folder first.".to_string()),
        Scope::Source => s.run.as_ref().and_then(|r| r.source.clone()).ok_or_else(|| NO_RUN.to_string()),
        Scope::Staging => s.run.as_ref().map(|r| r.staging.clone()).ok_or_else(|| NO_RUN.to_string()),
        Scope::Vault => s.run.as_ref().map(|r| r.root.clone()).ok_or_else(|| NO_RUN.to_string()),
    })?;
    Ok(Place::new(vpath, base))
}

#[tauri::command]
pub async fn import_fs_read_dir(path: String) -> Result<Vec<EntryInfo>, String> {
    let place = place(&path)?;
    blocking(move || files::read_dir(&place)).await
}

#[tauri::command]
pub async fn import_fs_read_text(path: String) -> Result<String, String> {
    let place = place(&path)?;
    blocking(move || files::read_text(&place)).await
}

#[tauri::command]
pub async fn import_fs_stat(path: String) -> Result<StatInfo, String> {
    let place = place(&path)?;
    blocking(move || files::stat(&place)).await
}

#[tauri::command]
pub async fn import_fs_write_text(path: String, text: String, exclusive: bool) -> Result<(), String> {
    let place = place(&path)?;
    blocking(move || files::write_text(&place, &text, exclusive)).await
}

#[tauri::command]
pub async fn import_fs_append_text(path: String, text: String) -> Result<(), String> {
    let place = place(&path)?;
    let moved_in = session::with_session(place.kind(), |s| {
        s.run.as_ref().map(|r| r.moved_in.clone()).unwrap_or_default()
    });
    blocking(move || files::append_text(&place, &text, &moved_in)).await
}

#[tauri::command]
pub async fn import_fs_mkdir(path: String) -> Result<(), String> {
    let place = place(&path)?;
    blocking(move || files::mkdir(&place)).await
}

#[tauri::command]
pub async fn import_fs_copy(from: String, to: String) -> Result<(), String> {
    let (from, to) = (place(&from)?, place(&to)?);
    blocking(move || files::copy(&from, &to)).await
}

#[tauri::command]
pub async fn import_fs_utimes(path: String, atime_ms: f64, mtime_ms: f64) -> Result<(), String> {
    let place = place(&path)?;
    blocking(move || files::utimes(&place, atime_ms, mtime_ms)).await
}

#[tauri::command]
pub async fn import_fs_rm(path: String) -> Result<(), String> {
    let place = place(&path)?;
    blocking(move || files::remove(&place)).await
}

#[tauri::command]
pub async fn import_fs_rename(from: String, to: String) -> Result<(), String> {
    let (from, to) = (place(&from)?, place(&to)?);
    blocking(move || files::rename(&from, &to)).await
}

/// Upstream's `moveIntoVault`. The folder made is the one place the run may
/// append to afterwards (Logseq's report).
#[tauri::command]
pub async fn import_fs_move_into_vault(from: String, to: String) -> Result<(), String> {
    let (from, to) = (place(&from)?, place(&to)?);
    let kind = to.kind();
    let moved = blocking(move || files::move_into_vault(&from, &to)).await?;
    session::with_session(kind, |s| {
        if let Some(run) = s.run.as_mut() {
            run.moved_in.push(moved);
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn import_write_note_created_at(path: String, created_at: f64) -> Result<(), String> {
    let place = place(&path)?;
    blocking(move || files::write_note_created_at(&place, created_at)).await
}

// ---- Notion -----------------------------------------------------------------

/// `NOTION_IMPORT_STATUS`.
#[tauri::command]
pub async fn notion_import_status(
    app: AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    page: String,
) -> Result<Value, String> {
    reap(&app, Kind::Notion, window.label(), &page);
    let remote = state.remote_session().is_some();
    let root = if remote { None } else { state.current_root() };
    let (running, progress) = session::with_session(Kind::Notion, |s| (s.run.is_some(), s.progress.clone()));
    let staging = match root {
        Some(root) => Some(staging_for(&app, Kind::Notion, &root)?),
        None => None,
    };
    let (signed_in, token_set, can_store, resumable) = blocking(move || {
        let resumable = staging
            .map(|dir| fs::read_dir(dir.join("records")).map(|mut d| d.next().is_some()).unwrap_or(false))
            .unwrap_or(false);
        Ok((notion_auth::signed_in(), notion_auth::token_is_set(), notion_auth::can_store(), resumable))
    })
    .await?;
    Ok(json!({
        "signedIn": signed_in,
        "tokenSet": token_set,
        "canStore": can_store,
        "remote": remote,
        "running": running,
        "resumable": resumable,
        "progress": progress,
    }))
}

/// `NOTION_IMPORT_DISCARD`: throws away what a stopped run read.
#[tauri::command]
pub async fn notion_import_discard(
    app: AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    page: String,
) -> Result<(), String> {
    if state.remote_session().is_some() {
        return Ok(());
    }
    let root = local_root(&state)?;
    reap(&app, Kind::Notion, window.label(), &page);
    session::with_session(Kind::Notion, |s| {
        if s.run.is_some() {
            return Err("Stop the import first.".to_string());
        }
        s.progress = None;
        Ok(())
    })?;
    let staging = staging_for(&app, Kind::Notion, &root)?;
    blocking(move || match fs::remove_dir_all(&staging) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
        _ => Ok(()),
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionCredentials {
    signed_in: bool,
    token_set: bool,
}

/// Which client the run builds (upstream's `clientFor`).
#[tauri::command]
pub async fn notion_credentials() -> Result<NotionCredentials, String> {
    blocking(|| Ok(NotionCredentials { signed_in: notion_auth::signed_in(), token_set: notion_auth::token_is_set() })).await
}

/// Keeps a token the webview has just checked with Notion.
#[tauri::command]
pub async fn notion_token_set(token: String) -> Result<bool, String> {
    blocking(move || Ok(notion_auth::set_token(&token))).await
}

/// `NOTION_IMPORT_SIGN_OUT`.
#[tauri::command]
pub async fn notion_sign_out() -> Result<(), String> {
    blocking(|| {
        notion_auth::sign_out();
        Ok(())
    })
    .await
}

/// `NOTION_IMPORT_SIGN_IN`: resolves once Notion has answered and the session
/// is stored, and brings the window back from the browser.
#[tauri::command]
pub async fn notion_sign_in(window: tauri::Window) -> Result<(), String> {
    blocking(notion_auth::sign_in).await?;
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

/// `NOTION_IMPORT_CANCEL_SIGN_IN`.
#[tauri::command]
pub async fn notion_sign_in_cancel() -> Result<(), String> {
    notion_auth::cancel_sign_in();
    Ok(())
}

/// The MCP transport's `refreshToken`: whether a fresh token is in place.
#[tauri::command]
pub async fn notion_sign_in_refresh() -> Result<bool, String> {
    blocking(|| notion_auth::refresh().map(|fresh| fresh.is_some())).await
}

#[tauri::command]
pub async fn notion_api_request(
    url: String,
    method: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    token: Option<String>,
) -> Result<HostReply, String> {
    blocking(move || notion_http::api_request(&url, &method, &headers, body.as_deref(), token)).await
}

#[tauri::command]
pub async fn notion_mcp_request(
    url: String,
    method: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
) -> Result<HostReply, String> {
    blocking(move || notion_http::mcp_request(&url, &method, &headers, body.as_deref())).await
}

/// Upstream's `notifyIfAway`: a run can take hours, and nobody watches a
/// progress bar that long. Shown only while no window has focus.
#[tauri::command]
pub async fn import_notify_if_away(app: AppHandle, title: String, body: String) -> Result<(), String> {
    if app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false)) {
        return Ok(());
    }
    let clip = |text: &str| text.chars().take(400).collect::<String>();
    let (title, body) = (clip(&title), clip(&body));
    let identifier = app.config().identifier.clone();
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification.summary(&title).body(&body).appname("ZenNotes-rs");
        #[cfg(target_os = "windows")]
        notification.app_id(&identifier);
        #[cfg(target_os = "macos")]
        let _ = notify_rust::set_application(&identifier);
        let _ = &identifier;
        if let Err(error) = notification.show() {
            eprintln!("imports: could not show a notification: {error}");
        }
    });
    Ok(())
}

/// Upstream's `allowInsecureFiles: usingStandIn()`.
#[tauri::command]
pub async fn notion_import_stand_in() -> Result<bool, String> {
    Ok(notion_http::using_stand_in())
}

/// The staging store's download (notion-import-run.ts), into staging only.
#[tauri::command]
pub async fn import_notion_download(url: String, target: String, refuse_html: bool) -> Result<DownloadReply, String> {
    let place = place(&target)?;
    if place.kind() != Kind::Notion || place.scope() != Scope::Staging {
        return Err(place.denied());
    }
    place.check_no_links()?;
    let stop = session::with_session(Kind::Notion, |s| s.run.as_ref().map(|r| r.stop.clone()))
        .ok_or_else(|| NO_RUN.to_string())?;
    let allow_insecure = notion_http::using_stand_in();
    blocking(move || notion_http::download(&url, &place.abs, refuse_html, allow_insecure, &stop)).await
}
