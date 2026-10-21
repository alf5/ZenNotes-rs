//! Vault settings (`.zennotes/vault.json`) — port of the settings helpers in
//! apps/desktop/src/main/vault.ts (`getVaultSettings`, `setVaultSettings`,
//! `normalizeVaultSettings`, `inferPrimaryNotesLocation`, folder-icon rewrites).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::ipc::types::{PeriodicNotesSettings, VaultInfo, VaultSettings};
use crate::vault::layout::{
    is_reserved_root_name, vault_info, INTERNAL_VAULT_DIR, VAULT_SETTINGS_FILE,
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

/// Longer than any name that fits a sidebar header. Counted in UTF-16 code
/// units, like `String.length` upstream.
const VAULT_DISPLAY_NAME_MAX_LENGTH: usize = 64;

/// JavaScript's `\s` class, which the upstream rules are written in.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// Port of shared-domain `normalizeVaultDisplayName` (#692): control
/// characters dropped (not the whitespace ones), whitespace runs collapsed,
/// trimmed, cut at the limit without splitting a surrogate pair. None when
/// nothing usable is left, so vault.json is written without the key.
pub fn normalize_vault_display_name(raw: Option<&Value>) -> Option<String> {
    let text = raw?.as_str()?;
    let mut cleaned = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        let code = c as u32;
        if code <= 0x08 || (0x0e..=0x1f).contains(&code) || (0x7f..=0x9f).contains(&code) {
            continue;
        }
        if is_js_space(c) {
            pending_space = true;
            continue;
        }
        if pending_space && !cleaned.is_empty() {
            cleaned.push(' ');
        }
        pending_space = false;
        cleaned.push(c);
    }
    if cleaned.is_empty() {
        return None;
    }
    let mut units = 0;
    let mut cut = String::with_capacity(cleaned.len());
    for c in cleaned.chars() {
        if units + c.len_utf16() > VAULT_DISPLAY_NAME_MAX_LENGTH {
            break;
        }
        units += c.len_utf16();
        cut.push(c);
    }
    let cut = cut.trim_end_matches(is_js_space).to_string();
    (!cut.is_empty()).then_some(cut)
}

/// Port of `describeVault`: the vault as the app names it, its display name
/// from vault.json when it has one, else the folder name.
pub fn describe_vault(root: &Path) -> VaultInfo {
    let mut info = vault_info(root);
    let display = fs::read_to_string(settings_path(root))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| normalize_vault_display_name(value.get("displayName")));
    if let Some(name) = display {
        info.name = name;
    }
    info
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
    // Carry every other key verbatim (titlePattern, locale, legacyPatterns,
    // tasksDueOnNoteDate, …) — the renderer's normalizer owns their validation.
    let mut extra = serde_json::Map::new();
    if let Some(o) = obj {
        for (key, value) in o {
            if !matches!(key.as_str(), "enabled" | "directory" | "templateId") {
                extra.insert(key.clone(), value.clone());
            }
        }
    }
    PeriodicNotesSettings { enabled, directory, template_id, extra }
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
                extra: serde_json::Map::new(),
            },
            weekly_notes: PeriodicNotesSettings {
                enabled: false,
                directory: DEFAULT_WEEKLY_NOTES_DIRECTORY.into(),
                template_id: None,
                extra: serde_json::Map::new(),
            },
            folder_icons: BTreeMap::new(),
            extra: serde_json::Map::new(),
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

    // v2.15+ keys this backend doesn't interpret (monthlyNotes, folderColors,
    // favorites, view, *Location, …) pass through verbatim so a get → set
    // round-trip never drops them.
    let mut extra = serde_json::Map::new();
    for (key, value) in obj {
        if !matches!(
            key.as_str(),
            "primaryNotesLocation" | "dailyNotes" | "weeklyNotes" | "folderIcons" | "displayName"
        ) {
            extra.insert(key.clone(), value.clone());
        }
    }
    // Omitted rather than stored empty, so a vault that never named itself
    // keeps a vault.json without the key (#692).
    if let Some(name) = normalize_vault_display_name(obj.get("displayName")) {
        extra.insert("displayName".into(), Value::String(name));
    }

    VaultSettings {
        primary_notes_location: primary.to_string(),
        daily_notes: normalize_periodic(obj.get("dailyNotes"), DEFAULT_DAILY_NOTES_DIRECTORY),
        weekly_notes: normalize_periodic(obj.get("weeklyNotes"), DEFAULT_WEEKLY_NOTES_DIRECTORY),
        folder_icons,
        extra,
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

// ---- settings that name folders, after a folder rename or delete ----------
// Ports of `rewriteFolderIconsForRename` / `rewriteFolderColorsForRename` /
// `removeFolderIcons` / `removeFolderColors` (vault.ts) and
// `rewriteTasksFoldersForRename` (shared-domain tasks-excluded-folders.ts),
// applied to the whole settings document so the result can be persisted in
// the same transaction as the folder move.

/// An entry keyed `folder:subpath` (or below it) follows the folder.
fn rewrite_keyed_map(map: &serde_json::Map<String, Value>, folder: &str, old_sub: &str, new_sub: &str) -> serde_json::Map<String, Value> {
    let exact = folder_icon_key(folder, old_sub);
    let prefix = format!("{exact}/");
    let renamed = folder_icon_key(folder, new_sub);
    let mut next = serde_json::Map::new();
    for (key, value) in map {
        let key = if key == &exact {
            renamed.clone()
        } else if key.starts_with(&prefix) {
            format!("{renamed}{}", &key[exact.len()..])
        } else {
            key.clone()
        };
        next.insert(key, value.clone());
    }
    next
}

fn remove_keyed(map: &serde_json::Map<String, Value>, folder: &str, sub: &str) -> serde_json::Map<String, Value> {
    let exact = folder_icon_key(folder, sub);
    let prefix = format!("{exact}/");
    map.iter()
        .filter(|(key, _)| *key != &exact && !key.starts_with(&prefix))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Port of `normalizeTasksExcludedFolder`.
fn normalize_tasks_folder(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let mut parts = Vec::new();
    for segment in text.replace('\\', "/").split('/') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return None;
        }
        parts.push(segment.to_string());
    }
    let joined = parts.join("/");
    (!joined.is_empty() && joined.encode_utf16().count() <= 512).then_some(joined)
}

/// Port of `rewriteFolderList`: an entry naming the folder, or a folder
/// inside it, follows it; the list is re-normalized (deduped, order kept).
fn rewrite_tasks_folder_list(list: &[Value], old_rel: &str, new_rel: &str) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in list {
        let moved = match entry.as_str() {
            Some(text) if text == old_rel || text.starts_with(&format!("{old_rel}/")) => {
                Value::String(format!("{new_rel}{}", &text[old_rel.len()..]))
            }
            _ => entry.clone(),
        };
        if let Some(clean) = normalize_tasks_folder(&moved) {
            if seen.insert(clean.clone()) {
                out.push(Value::String(clean));
            }
        }
    }
    out
}

/// The settings document after `folder:old_sub` (vault-relative `old_rel`)
/// was renamed or moved to `folder:new_sub` (`new_rel`).
pub fn after_folder_rename(mut value: Value, folder: &str, old_sub: &str, new_sub: &str, old_rel: &str, new_rel: &str) -> Value {
    for key in ["folderIcons", "folderColors"] {
        if let Some(map) = value.get(key).and_then(Value::as_object) {
            value[key] = Value::Object(rewrite_keyed_map(map, folder, old_sub, new_sub));
        }
    }
    // The Tasks folder lists name on-disk paths, so they follow the folder too.
    if !old_rel.is_empty() && !new_rel.is_empty() && old_rel != new_rel {
        if let Some(tasks) = value.get_mut("tasks").and_then(Value::as_object_mut) {
            for key in ["excludedFolders", "includedFolders"] {
                if let Some(list) = tasks.get(key).and_then(Value::as_array) {
                    let next = rewrite_tasks_folder_list(list, old_rel, new_rel);
                    tasks.insert(key.to_string(), Value::Array(next));
                }
            }
        }
    }
    value
}

/// The settings document after `folder:sub` was deleted.
pub fn after_folder_delete(mut value: Value, folder: &str, sub: &str) -> Value {
    for key in ["folderIcons", "folderColors"] {
        if let Some(map) = value.get(key).and_then(Value::as_object) {
            value[key] = Value::Object(remove_keyed(map, folder, sub));
        }
    }
    value
}

/// The current settings as the JSON document `set_vault_settings` takes.
pub fn settings_value(root: &Path) -> Result<Value, String> {
    serde_json::to_value(get_vault_settings(root)).map_err(|e| e.to_string())
}

// ---- folder-icon rewrites (ported from vault.ts) -------------------------

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

    fn name(raw: &str) -> Option<String> {
        normalize_vault_display_name(Some(&Value::String(raw.into())))
    }

    /// The cases of shared-domain vault-display-name.test.ts, which the rules
    /// must stay byte-compatible with.
    #[test]
    fn display_name_matches_shared_domain_rules() {
        assert_eq!(name("Acme API docs").as_deref(), Some("Acme API docs"));
        assert_eq!(name("Été 2026 · notes").as_deref(), Some("Été 2026 · notes"));
        assert_eq!(name("  Acme   API\tdocs \n").as_deref(), Some("Acme API docs"));
        assert_eq!(name("Acme\u{0} docs\u{2028}v2\u{7}").as_deref(), Some("Acme docs v2"));
        assert_eq!(name("A \u{7} B").as_deref(), Some("A B"));
        assert_eq!(name(""), None);
        assert_eq!(name("   "), None);
        assert_eq!(name("\u{7}"), None);
        assert_eq!(normalize_vault_display_name(None), None);
        assert_eq!(normalize_vault_display_name(Some(&serde_json::json!(42))), None);

        let long = format!("{}end", "word ".repeat(20));
        let out = name(&long).unwrap();
        assert!(out.encode_utf16().count() <= 64);
        assert_eq!(out, out.trim_end());
        assert_eq!(name(&"x".repeat(64)).unwrap().len(), 64);

        // Never splits a surrogate pair at the cut.
        assert_eq!(name(&format!("{}😀tail", "x".repeat(63))), Some("x".repeat(63)));
        assert_eq!(
            name(&format!("{}😀tail", "x".repeat(62))),
            Some(format!("{}😀", "x".repeat(62)))
        );
    }

    #[test]
    fn display_name_is_stored_normalized_and_names_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        crate::vault::layout::ensure_vault_layout(dir.path()).unwrap();
        let folder = dir.path().file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(describe_vault(dir.path()).name, folder);

        let mut next = serde_json::to_value(get_vault_settings(dir.path())).unwrap();
        next["displayName"] = Value::String("  Acme \t docs ".into());
        let saved = set_vault_settings(dir.path(), &next).unwrap();
        assert_eq!(saved.extra.get("displayName"), Some(&Value::String("Acme docs".into())));
        assert_eq!(describe_vault(dir.path()).name, "Acme docs");

        // Clearing it is the same as never having set it: the key goes.
        next["displayName"] = Value::String("   ".into());
        let cleared = set_vault_settings(dir.path(), &next).unwrap();
        assert!(!cleared.extra.contains_key("displayName"));
        assert_eq!(describe_vault(dir.path()).name, folder);
    }

    #[test]
    fn defaults_when_missing() {
        let s = normalize_vault_settings(None, "inbox");
        assert_eq!(s.primary_notes_location, "inbox");
        assert!(!s.daily_notes.enabled);
        assert_eq!(s.daily_notes.directory, "Daily Notes");
        assert!(s.folder_icons.is_empty());
    }

    #[test]
    fn folder_rename_rewrites_icons_colors_and_task_folders() {
        let value = serde_json::json!({
            "folderIcons": { "inbox:Work": "briefcase", "inbox:Work/Sub": "bolt", "inbox:Other": "star" },
            "folderColors": { "inbox:Work/Sub": "teal", "archive:Work": "red" },
            "tasks": {
                "excludedFolders": ["inbox/Work/Old", "inbox/Workshop"],
                "includedFolders": ["inbox/Work", "inbox/Job"]
            }
        });
        let next = after_folder_rename(value, "inbox", "Work", "Job", "inbox/Work", "inbox/Job");
        assert_eq!(next["folderIcons"]["inbox:Job"], "briefcase");
        assert_eq!(next["folderIcons"]["inbox:Job/Sub"], "bolt");
        assert_eq!(next["folderIcons"]["inbox:Other"], "star");
        assert_eq!(next["folderColors"]["inbox:Job/Sub"], "teal");
        // Another top folder's key of the same name is not this folder.
        assert_eq!(next["folderColors"]["archive:Work"], "red");
        assert_eq!(next["tasks"]["excludedFolders"], serde_json::json!(["inbox/Job/Old", "inbox/Workshop"]));
        // A renamed Task folder lands on one already listed and collapses.
        assert_eq!(next["tasks"]["includedFolders"], serde_json::json!(["inbox/Job"]));

        let deleted = after_folder_delete(next, "inbox", "Job");
        assert_eq!(deleted["folderIcons"], serde_json::json!({ "inbox:Other": "star" }));
        assert_eq!(deleted["folderColors"], serde_json::json!({ "archive:Work": "red" }));
    }

    #[test]
    fn v215_keys_survive_a_set_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let input = serde_json::json!({
            "primaryNotesLocation": "root",
            "dailyNotes": { "enabled": true, "directory": "Daily", "titlePattern": "yyyy-MM-dd" },
            "weeklyNotes": { "enabled": false, "directory": "Weekly Notes" },
            "monthlyNotes": { "enabled": false, "directory": "Monthly Notes" },
            "folderIcons": {},
            "folderColors": { "inbox:Projects": "teal" },
            "favorites": ["inbox/Idea.md", "inbox:Projects"],
            "view": { "noteSortOrder": "title" }
        });
        set_vault_settings(dir.path(), &input).unwrap();
        let out = serde_json::to_value(get_vault_settings(dir.path())).unwrap();
        assert_eq!(out["monthlyNotes"]["directory"], "Monthly Notes");
        assert_eq!(out["folderColors"]["inbox:Projects"], "teal");
        assert_eq!(out["favorites"][0], "inbox/Idea.md");
        assert_eq!(out["view"]["noteSortOrder"], "title");
        assert_eq!(out["dailyNotes"]["titlePattern"], "yyyy-MM-dd");
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
