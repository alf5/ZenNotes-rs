//! OS integration — reveal-in-file-manager, app zoom, system fonts, and the
//! app icon. Ports the corresponding pieces of apps/desktop/src/main/index.ts.

use std::path::{Path, PathBuf};

use base64::Engine;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;
use crate::vault::layout::{self, PRIMARY_ATTACHMENTS_DIR};
use crate::vault::notes::resolve_safe;

const APP_ICON: &[u8] = include_bytes!("../icons/128x128.png");

fn require_root(state: &AppState) -> Result<PathBuf, String> {
    state.current_root().ok_or_else(|| "No vault is open".to_string())
}

fn reveal(app: &AppHandle, path: &Path) -> Result<(), String> {
    app.opener()
        .reveal_item_in_dir(path.to_path_buf())
        .map_err(|e| format!("Failed to reveal: {e}"))
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn reveal_note(app: &AppHandle, state: &AppState, rel: &str, follow_symlink: bool) -> Result<(), String> {
    let abs = resolve_safe(&require_root(state)?, rel)?;
    reveal(app, &if follow_symlink { canonical_or_self(&abs) } else { abs })
}

pub fn reveal_folder(
    app: &AppHandle,
    state: &AppState,
    folder: &str,
    subpath: &str,
    follow_symlink: bool,
) -> Result<(), String> {
    let root = require_root(state)?;
    let top = layout::folder_root(&root, folder);
    let clean = subpath.trim_matches('/');
    let abs = if clean.is_empty() { top } else { resolve_safe(&top, clean)? };
    reveal(app, &if follow_symlink { canonical_or_self(&abs) } else { abs })
}

pub fn reveal_assets_dir(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let dir = require_root(state)?.join(PRIMARY_ATTACHMENTS_DIR);
    let _ = std::fs::create_dir_all(&dir);
    reveal(app, &dir)
}

/// Apply a zoom factor to every open window and persist it.
pub fn apply_zoom(app: &AppHandle, factor: f64) {
    for (_, win) in app.webview_windows() {
        let _ = win.set_zoom(factor);
    }
}

/// Enumerate system font family names, sorted and de-duped. Falls back to a
/// baseline list if enumeration fails.
pub fn list_system_fonts() -> Vec<String> {
    use font_kit::source::SystemSource;
    let source = SystemSource::new();
    match source.all_families() {
        Ok(mut families) => {
            families.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
            families.dedup();
            families
        }
        Err(_) => baseline_fonts(),
    }
}

fn baseline_fonts() -> Vec<String> {
    ["Inter", "SF Pro Text", "Helvetica Neue", "Arial", "Georgia", "Times New Roman", "JetBrains Mono", "SF Mono", "Menlo", "Courier New"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The app icon as a `data:image/png;base64,...` URL.
pub fn icon_data_url() -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(APP_ICON);
    format!("data:image/png;base64,{b64}")
}
