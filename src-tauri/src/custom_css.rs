//! Custom themes + CSS overrides — the file layer under
//! src/bridge/custom-css.ts. Ports the fs half of upstream
//! `apps/desktop/src/main/{custom-themes,overrides}.ts`; manifest parsing and
//! theme scaffolding stay in the webview (vendored shared-domain functions).
//!
//! Layout (siblings of config.toml, see app_config::config_dir):
//!   themes/<slug>/{manifest.json, theme.css, assets…}
//!   overrides/*.css
//!
//! Watchers emit content-free pings (`custom-themes://changed`,
//! `overrides://changed`); the frontend re-scans and fans out fresh lists.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify_debouncer_full::notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use serde::Serialize;
use tauri::http::{Request, Response};
use tauri::{AppHandle, Emitter};

use crate::app_config::config_dir;
use crate::watcher::VaultDebouncer;

const THEMES_CHANGED_EVENT: &str = "custom-themes://changed";
const OVERRIDES_CHANGED_EVENT: &str = "overrides://changed";
const WATCH_DEBOUNCE_MS: u64 = 200;

/// Keeps the two dir debouncers alive for the app's lifetime.
pub struct CustomCssWatchers(#[allow(dead_code)] pub Mutex<Vec<VaultDebouncer>>);

fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

fn overrides_dir() -> PathBuf {
    config_dir().join("overrides")
}

/// A bare slug/name resolving to a direct child of its dir (no traversal).
fn is_safe_slug(slug: &str) -> bool {
    !slug.is_empty() && !slug.contains('/') && !slug.contains('\\') && !slug.contains("..")
}

fn is_safe_override_name(name: &str) -> bool {
    is_safe_slug(name) && name.to_lowercase().ends_with(".css")
}

/// One theme folder, unparsed: the frontend runs `parseThemeManifest` and
/// builds the error stub when `css` is absent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThemeEntry {
    pub slug: String,
    pub css: Option<String>,
    pub manifest: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverrideEntry {
    pub name: String,
    pub css: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Port of upstream `resolveThemeAssetPath`: a file inside this theme's folder,
/// symlinks followed before the check, or nothing.
fn resolve_theme_asset_path(themes: &Path, slug: &str, rel: &str) -> Option<PathBuf> {
    if !is_safe_slug(slug) || rel.is_empty() {
        return None;
    }
    let decoded = urlencoding::decode(rel).ok()?.replace('\\', "/");
    let decoded = decoded.trim_start_matches('/');
    if decoded.is_empty() || decoded.contains('\0') || Path::new(decoded).is_absolute() {
        return None;
    }
    let root = themes.join(slug).canonicalize().ok()?;
    let real = root.join(decoded).canonicalize().ok()?;
    (real != root && real.starts_with(&root) && real.is_file()).then_some(real)
}

/// Slug and file from a theme asset URL's parts: `zen-theme://<slug>/<file>`,
/// or `http://zen-theme.localhost/<slug>/<file>` where WebView2 serves custom
/// schemes that way (src/bridge/custom-css.ts rewrites the CSS for it).
fn theme_asset_parts(host: &str, path: &str) -> Option<(String, String)> {
    let path = path.trim_start_matches('/');
    let (slug, rel) = if host.is_empty() || host == "zen-theme.localhost" {
        path.split_once('/')?
    } else {
        (host, path)
    };
    Some((urlencoding::decode(slug).ok()?.into_owned(), rel.to_string()))
}

/// `zen-theme://` — fonts and images referenced by a custom theme's CSS,
/// sandboxed to that theme's folder (upstream's `protocol.handle`).
pub fn handle_theme_asset(request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let not_found = || Response::builder().status(404).body(Vec::new()).unwrap();
    let uri = request.uri();
    let Some((slug, rel)) = theme_asset_parts(uri.host().unwrap_or(""), uri.path()) else {
        return not_found();
    };
    let Some(abs) = resolve_theme_asset_path(&themes_dir(), &slug, &rel) else {
        return not_found();
    };
    match fs::read(&abs) {
        Ok(bytes) => Response::builder()
            .header("Content-Type", crate::asset_protocol::mime_type_for_path(&abs.to_string_lossy()))
            .header("Cache-Control", "no-cache")
            .header("Access-Control-Allow-Origin", "*")
            .body(bytes)
            .unwrap(),
        Err(_) => not_found(),
    }
}

#[tauri::command]
pub fn custom_themes_scan() -> Vec<RawThemeEntry> {
    let dir = themes_dir();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let slug = entry.file_name().to_string_lossy().to_string();
        if slug.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let folder = entry.path();
        out.push(RawThemeEntry {
            css: fs::read_to_string(folder.join("theme.css")).ok(),
            manifest: fs::read_to_string(folder.join("manifest.json")).ok(),
            slug,
        });
    }
    out
}

#[tauri::command]
pub fn custom_themes_dir_path() -> String {
    themes_dir().to_string_lossy().to_string()
}

/// Reveal a theme's `theme.css` when the slug is valid and exists, else its
/// folder, else the themes dir (created on demand, like upstream's ensure).
#[tauri::command]
pub fn custom_themes_reveal(app: AppHandle, slug: Option<String>) -> Result<(), String> {
    let dir = themes_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let mut target = dir.clone();
    if let Some(slug) = slug.as_deref().filter(|s| is_safe_slug(s)) {
        let folder = dir.join(slug);
        if folder.parent() == Some(dir.as_path()) && folder.is_dir() {
            let css = folder.join("theme.css");
            target = if css.is_file() { css } else { folder };
        }
    }
    crate::os::reveal_file_path(&app, &target.to_string_lossy())
}

#[tauri::command]
pub fn custom_themes_delete(slug: String) -> Result<(), String> {
    if !is_safe_slug(&slug) {
        return Ok(());
    }
    let dir = themes_dir();
    let folder = dir.join(&slug);
    if folder.parent() != Some(dir.as_path()) {
        return Ok(());
    }
    let _ = fs::remove_dir_all(folder);
    Ok(())
}

/// Create a unique `<slug_base>[-N]` folder and return the final slug. The
/// frontend then scaffolds the CSS (which embeds the slug) and writes the
/// files via `custom_themes_write_files`.
#[tauri::command]
pub fn custom_themes_reserve(slug_base: String) -> Result<String, String> {
    if !is_safe_slug(&slug_base) {
        return Err("Invalid theme name.".into());
    }
    let dir = themes_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let mut slug = slug_base.clone();
    let mut n = 2;
    while dir.join(&slug).exists() {
        slug = format!("{slug_base}-{n}");
        n += 1;
    }
    let folder = dir.join(&slug);
    if folder.parent() != Some(dir.as_path()) {
        return Err("Invalid theme name.".into());
    }
    fs::create_dir_all(&folder).map_err(|e| format!("mkdir failed: {e}"))?;
    Ok(slug)
}

#[tauri::command]
pub fn custom_themes_write_files(slug: String, manifest: String, css: String) -> Result<(), String> {
    if !is_safe_slug(&slug) {
        return Err("Invalid theme slug.".into());
    }
    let dir = themes_dir();
    let folder = dir.join(&slug);
    if folder.parent() != Some(dir.as_path()) {
        return Err("Invalid theme slug.".into());
    }
    fs::create_dir_all(&folder).map_err(|e| format!("mkdir failed: {e}"))?;
    fs::write(folder.join("manifest.json"), manifest).map_err(|e| format!("write failed: {e}"))?;
    fs::write(folder.join("theme.css"), css).map_err(|e| format!("write failed: {e}"))
}

#[tauri::command]
pub fn overrides_list() -> Vec<OverrideEntry> {
    let dir = overrides_dir();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !name.to_lowercase().ends_with(".css") || !entry.path().is_file()
        {
            continue;
        }
        match fs::read_to_string(entry.path()) {
            Ok(css) => out.push(OverrideEntry { name, css, error: None }),
            Err(_) => out.push(OverrideEntry {
                name,
                css: String::new(),
                error: Some("Could not read this file.".into()),
            }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[tauri::command]
pub fn overrides_reveal(app: AppHandle, name: Option<String>) -> Result<(), String> {
    let dir = overrides_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let mut target = dir.clone();
    if let Some(name) = name.as_deref().filter(|n| is_safe_override_name(n)) {
        let file = dir.join(name);
        if file.parent() == Some(dir.as_path()) && file.is_file() {
            target = file;
        }
    }
    crate::os::reveal_file_path(&app, &target.to_string_lossy())
}

#[tauri::command]
pub fn overrides_delete(name: String) -> Result<(), String> {
    if !is_safe_override_name(&name) {
        return Ok(());
    }
    let dir = overrides_dir();
    let file = dir.join(&name);
    if file.parent() != Some(dir.as_path()) {
        return Ok(());
    }
    let _ = fs::remove_file(file);
    Ok(())
}

/// Watch both dirs (themes recursively for `<slug>/theme.css` edits, overrides
/// flat) and ping the frontend, which re-scans. Mirrors upstream's chokidar
/// setup (200 ms debounce, all event kinds).
pub fn spawn_watchers(app: AppHandle) -> Result<Vec<VaultDebouncer>, String> {
    let mut out = Vec::new();
    for (dir, event, recursive) in [
        (themes_dir(), THEMES_CHANGED_EVENT, RecursiveMode::Recursive),
        (overrides_dir(), OVERRIDES_CHANGED_EVENT, RecursiveMode::NonRecursive),
    ] {
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
        let handle = app.clone();
        let mut debouncer = new_debouncer(
            Duration::from_millis(WATCH_DEBOUNCE_MS),
            None,
            move |result: DebounceEventResult| {
                if result.is_ok() {
                    let _ = handle.emit(event, ());
                }
            },
        )
        .map_err(|e| format!("Failed to create watcher: {e}"))?;
        debouncer
            .watcher()
            .watch(&dir, recursive)
            .map_err(|e| format!("Failed to watch {}: {e}", dir.display()))?;
        debouncer.cache().add_root(&dir, recursive);
        out.push(debouncer);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_asset_urls_split_on_every_platform() {
        assert_eq!(
            theme_asset_parts("soft-paper", "/fonts/a%20b.woff2"),
            Some(("soft-paper".to_string(), "fonts/a%20b.woff2".to_string()))
        );
        assert_eq!(
            theme_asset_parts("zen-theme.localhost", "/soft-paper/a.woff2"),
            Some(("soft-paper".to_string(), "a.woff2".to_string()))
        );
        assert_eq!(theme_asset_parts("zen-theme.localhost", "/soft-paper"), None);
    }

    #[test]
    fn theme_assets_stay_inside_their_theme() {
        let themes = std::env::temp_dir().join(format!("zn-themes-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(themes.join("soft/fonts")).unwrap();
        fs::create_dir_all(themes.join("other")).unwrap();
        fs::write(themes.join("soft/fonts/a b.woff2"), b"font").unwrap();
        fs::write(themes.join("other/secret.png"), b"x").unwrap();
        fs::write(themes.join("outside.png"), b"x").unwrap();

        let found = resolve_theme_asset_path(&themes, "soft", "fonts/a%20b.woff2").unwrap();
        assert!(found.ends_with("soft/fonts/a b.woff2"));
        for (slug, rel) in [
            ("soft", "../other/secret.png"),
            ("soft", "..%2Fother%2Fsecret.png"),
            ("soft", "fonts"),
            ("soft", "missing.png"),
            ("soft", ""),
            ("..", "outside.png"),
            ("a/b", "x"),
        ] {
            assert_eq!(resolve_theme_asset_path(&themes, slug, rel), None, "{slug} {rel}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(themes.join("outside.png"), themes.join("soft/link.png")).unwrap();
            assert_eq!(resolve_theme_asset_path(&themes, "soft", "link.png"), None);
        }
        fs::remove_dir_all(themes).unwrap();
    }

    #[test]
    fn slug_guards_reject_traversal() {
        assert!(is_safe_slug("soft-paper"));
        assert!(!is_safe_slug(""));
        assert!(!is_safe_slug("../etc"));
        assert!(!is_safe_slug("a/b"));
        assert!(!is_safe_slug("a\\b"));
        assert!(is_safe_override_name("focus.css"));
        assert!(!is_safe_override_name("focus.txt"));
        assert!(!is_safe_override_name("../x.css"));
    }
}
