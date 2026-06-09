//! Vault settings (`.zennotes/vault.json`) — port of the settings helpers in
//! apps/desktop/src/main/vault.ts (`getVaultSettings`, `setVaultSettings`,
//! `normalizeVaultSettings`, `inferPrimaryNotesLocation`, folder-icon rewrites).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::ipc::types::{PeriodicNotesSettings, VaultSettings};
use crate::vault::layout::{
    is_reserved_root_name, INTERNAL_VAULT_DIR, VAULT_SETTINGS_FILE,
};

const DEFAULT_DAILY_NOTES_DIRECTORY: &str = "Daily Notes";
const DEFAULT_WEEKLY_NOTES_DIRECTORY: &str = "Weekly Notes";

const FOLDER_ICON_IDS: &[&str] = &[
    "folder", "bolt", "tray", "archive", "trash", "book", "bookmark", "calendar", "briefcase",
    "tag", "document", "sparkle", "code", "user", "star", "heart", "link", "lightbulb", "flask",
    "graduation", "music", "image", "palette", "terminal", "wrench", "globe", "map", "chart",
    "home",
];

fn is_folder_icon_id(value: &str) -> bool {
    FOLDER_ICON_IDS.contains(&value)
}

pub fn folder_icon_key(folder: &str, subpath: &str) -> String {
    format!("{folder}:{subpath}")
}

fn settings_path(root: &Path) -> std::path::PathBuf {
    root.join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE)
}

fn trim_slashes(s: &str) -> &str {
    s.trim_matches('/')
}

fn normalize_periodic(value: Option<&Value>, default_dir: &str) -> PeriodicNotesSettings {
    let obj = value.and_then(Value::as_object);
    let enabled = obj
        .and_then(|o| o.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let directory = obj
        .and_then(|o| o.get("directory"))
        .and_then(Value::as_str)
        .map(|s| trim_slashes(s.trim()).to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_dir.to_string());
    let template_id = obj
        .and_then(|o| o.get("templateId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    PeriodicNotesSettings { enabled, directory, template_id }
}

/// Port of `normalizeVaultSettings`.
pub fn normalize_vault_settings(value: Option<&Value>, fallback_primary: &str) -> VaultSettings {
    let obj = value.and_then(Value::as_object);
    let Some(obj) = obj else {
        return VaultSettings {
            primary_notes_location: fallback_primary.to_string(),
            daily_notes: PeriodicNotesSettings {
                enabled: false,
                directory: DEFAULT_DAILY_NOTES_DIRECTORY.into(),
                template_id: None,
            },
            weekly_notes: PeriodicNotesSettings {
                enabled: false,
                directory: DEFAULT_WEEKLY_NOTES_DIRECTORY.into(),
                template_id: None,
            },
            folder_icons: BTreeMap::new(),
        };
    };

    let mut folder_icons = BTreeMap::new();
    if let Some(map) = obj.get("folderIcons").and_then(Value::as_object) {
        for (key, icon) in map {
            if key.is_empty() {
                continue;
            }
            if let Some(icon) = icon.as_str() {
                if is_folder_icon_id(icon) {
                    folder_icons.insert(key.clone(), icon.to_string());
                }
            }
        }
    }

    let primary = match obj.get("primaryNotesLocation").and_then(Value::as_str) {
        Some("root") => "root",
        Some(_) => "inbox",
        None => fallback_primary,
    };

    VaultSettings {
        primary_notes_location: primary.to_string(),
        daily_notes: normalize_periodic(obj.get("dailyNotes"), DEFAULT_DAILY_NOTES_DIRECTORY),
        weekly_notes: normalize_periodic(obj.get("weeklyNotes"), DEFAULT_WEEKLY_NOTES_DIRECTORY),
        folder_icons,
    }
}

/// Port of `inferPrimaryNotesLocation`: a vault with top-level notes/dirs is
/// treated as "root mode".
pub fn infer_primary_notes_location(root: &Path) -> String {
    let Ok(entries) = fs::read_dir(root) else {
        return "inbox".to_string();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || is_reserved_root_name(&name) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            return "root".to_string();
        }
        if ft.is_file() && name.to_lowercase().ends_with(".md") {
            return "root".to_string();
        }
    }
    "inbox".to_string()
}

pub fn get_vault_settings(root: &Path) -> VaultSettings {
    let fallback = infer_primary_notes_location(root);
    match fs::read_to_string(settings_path(root)) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) => normalize_vault_settings(Some(&value), &fallback),
            Err(_) => normalize_vault_settings(None, &fallback),
        },
        Err(_) => normalize_vault_settings(None, &fallback),
    }
}

pub fn set_vault_settings(root: &Path, next: &Value) -> Result<VaultSettings, String> {
    let fallback = infer_primary_notes_location(root);
    let normalized = normalize_vault_settings(Some(next), &fallback);
    let path = settings_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let body = serde_json::to_string_pretty(&normalized).map_err(|e| e.to_string())?;
    fs::write(&path, body).map_err(|e| format!("write failed: {e}"))?;
    if normalized.primary_notes_location == "inbox" {
        let _ = fs::create_dir_all(root.join("inbox"));
    }
    Ok(normalized)
}

/// Apply a folder-icon transform and persist. Used by folder rename/delete/
/// duplicate (M5).
pub fn update_folder_icons<F>(root: &Path, transform: F) -> Result<(), String>
where
    F: FnOnce(&BTreeMap<String, String>) -> BTreeMap<String, String>,
{
    let settings = get_vault_settings(root);
    let next_icons = transform(&settings.folder_icons);
    let mut value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
    value["folderIcons"] = serde_json::to_value(&next_icons).map_err(|e| e.to_string())?;
    set_vault_settings(root, &value)?;
    Ok(())
}

// ---- folder-icon rewrites (ported from vault.ts) -------------------------

pub fn rewrite_folder_icons_for_rename(
    icons: &BTreeMap<String, String>,
    folder: &str,
    old_subpath: &str,
    new_subpath: &str,
) -> BTreeMap<String, String> {
    let exact = folder_icon_key(folder, old_subpath);
    let prefix = format!("{exact}/");
    let new_key = folder_icon_key(folder, new_subpath);
    let mut next = BTreeMap::new();
    for (key, value) in icons {
        if *key == exact {
            next.insert(new_key.clone(), value.clone());
        } else if let Some(rest) = key.strip_prefix(&prefix) {
            next.insert(format!("{new_key}/{rest}"), value.clone());
        } else {
            next.insert(key.clone(), value.clone());
        }
    }
    next
}

pub fn remove_folder_icons(
    icons: &BTreeMap<String, String>,
    folder: &str,
    subpath: &str,
) -> BTreeMap<String, String> {
    let exact = folder_icon_key(folder, subpath);
    let prefix = format!("{exact}/");
    icons
        .iter()
        .filter(|(key, _)| **key != exact && !key.starts_with(&prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn duplicate_folder_icons(
    icons: &BTreeMap<String, String>,
    folder: &str,
    source_subpath: &str,
    target_subpath: &str,
) -> BTreeMap<String, String> {
    let exact = folder_icon_key(folder, source_subpath);
    let prefix = format!("{exact}/");
    let target_key = folder_icon_key(folder, target_subpath);
    let mut next = icons.clone();
    for (key, value) in icons {
        if *key == exact {
            next.insert(target_key.clone(), value.clone());
        } else if let Some(rest) = key.strip_prefix(&prefix) {
            next.insert(format!("{target_key}/{rest}"), value.clone());
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing() {
        let s = normalize_vault_settings(None, "inbox");
        assert_eq!(s.primary_notes_location, "inbox");
        assert!(!s.daily_notes.enabled);
        assert_eq!(s.daily_notes.directory, "Daily Notes");
        assert!(s.folder_icons.is_empty());
    }

    #[test]
    fn folder_icon_rename_rewrites_prefix() {
        let mut icons = BTreeMap::new();
        icons.insert("inbox:Work".to_string(), "briefcase".to_string());
        icons.insert("inbox:Work/Sub".to_string(), "bolt".to_string());
        icons.insert("inbox:Other".to_string(), "star".to_string());
        let next = rewrite_folder_icons_for_rename(&icons, "inbox", "Work", "Job");
        assert_eq!(next.get("inbox:Job").map(String::as_str), Some("briefcase"));
        assert_eq!(next.get("inbox:Job/Sub").map(String::as_str), Some("bolt"));
        assert_eq!(next.get("inbox:Other").map(String::as_str), Some("star"));
    }

    #[test]
    fn set_then_get_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let input = serde_json::json!({
            "primaryNotesLocation": "inbox",
            "dailyNotes": { "enabled": true, "directory": "Journal" },
            "weeklyNotes": { "enabled": false, "directory": "Weekly Notes" },
            "folderIcons": { "inbox:Work": "briefcase", "inbox:Bad": "not-an-icon" }
        });
        set_vault_settings(dir.path(), &input).unwrap();
        let got = get_vault_settings(dir.path());
        assert!(got.daily_notes.enabled);
        assert_eq!(got.daily_notes.directory, "Journal");
        // Invalid icon id is dropped during normalization.
        assert_eq!(got.folder_icons.get("inbox:Work").map(String::as_str), Some("briefcase"));
        assert!(!got.folder_icons.contains_key("inbox:Bad"));
    }
}
