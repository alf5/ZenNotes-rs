//! Persisted application config — port of the `PersistedConfig` logic in
//! ZenNotes' apps/desktop/src/main/vault.ts.
//!
//! Stored as `synnotes.config.json` in the OS app-config dir
//! (`~/Library/Application Support/com.synnotes.app` on macOS). Unknown
//! fields round-trip untouched so future-version configs aren't clobbered.
//! A `.bak` sibling is kept for crash recovery (matches the Electron
//! backup-on-save behaviour the config tests assert).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ipc::types::{LocalVaultEntry, VaultInfo};

pub const CONFIG_FILE: &str = "synnotes.config.json";
pub const DEFAULT_QUICK_CAPTURE_HOTKEY: &str = "CommandOrControl+Shift+Space";
const MAX_REMEMBERED_VAULTS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedLocalVault {
    pub root: String,
    pub name: String,
    #[serde(default)]
    pub last_opened_at: i64,
}

/// We model the fields the Rust backend manages directly and keep everything
/// else (remote workspace, window state, …) as opaque JSON so it survives a
/// load → save round-trip until the owning milestone ports it.
#[derive(Debug, Clone)]
pub struct PersistedConfig {
    pub workspace_mode: String,
    pub vault_root: Option<String>,
    pub local_vaults: Vec<PersistedLocalVault>,
    pub zoom_factor: f64,
    pub quick_capture_hotkey: String,
    pub quick_capture_pinned: bool,
    /// Untouched passthrough for fields owned by later milestones.
    pub extra: Map<String, Value>,
}

impl Default for PersistedConfig {
    fn default() -> Self {
        Self {
            workspace_mode: "local".into(),
            vault_root: None,
            local_vaults: Vec::new(),
            zoom_factor: 1.0,
            quick_capture_hotkey: DEFAULT_QUICK_CAPTURE_HOTKEY.into(),
            quick_capture_pinned: false,
            extra: Map::new(),
        }
    }
}

impl PersistedConfig {
    fn from_value(value: &Value) -> Self {
        let mut cfg = PersistedConfig::default();
        let Some(obj) = value.as_object() else {
            return cfg;
        };
        let mut extra = obj.clone();

        if let Some(Value::String(mode)) = extra.remove("workspaceMode") {
            cfg.workspace_mode = if mode == "remote" { "remote".into() } else { "local".into() };
        }
        match extra.remove("vaultRoot") {
            Some(Value::String(root)) => cfg.vault_root = Some(root),
            _ => cfg.vault_root = None,
        }
        if let Some(Value::Number(n)) = extra.remove("zoomFactor") {
            if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    cfg.zoom_factor = (f * 100.0).round() / 100.0;
                    cfg.zoom_factor = cfg.zoom_factor.clamp(0.5, 3.0);
                }
            }
        }
        if let Some(Value::String(hotkey)) = extra.remove("quickCaptureHotkey") {
            cfg.quick_capture_hotkey = hotkey.trim().to_string();
        }
        if let Some(Value::Bool(pinned)) = extra.remove("quickCapturePinned") {
            cfg.quick_capture_pinned = pinned;
        }
        if let Some(local) = extra.remove("localVaults") {
            cfg.local_vaults = normalize_local_vaults(&local);
        }
        cfg.extra = extra;
        cfg
    }

    fn to_value(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("workspaceMode".into(), Value::String(self.workspace_mode.clone()));
        obj.insert(
            "vaultRoot".into(),
            self.vault_root.clone().map(Value::String).unwrap_or(Value::Null),
        );
        obj.insert(
            "localVaults".into(),
            serde_json::to_value(&self.local_vaults).unwrap_or(Value::Array(vec![])),
        );
        obj.insert(
            "zoomFactor".into(),
            serde_json::json!(self.zoom_factor),
        );
        obj.insert(
            "quickCaptureHotkey".into(),
            Value::String(self.quick_capture_hotkey.clone()),
        );
        obj.insert("quickCapturePinned".into(), Value::Bool(self.quick_capture_pinned));
        Value::Object(obj)
    }
}

fn normalize_local_vaults(value: &Value) -> Vec<PersistedLocalVault> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<PersistedLocalVault> = Vec::new();
    for raw in arr {
        let Some(obj) = raw.as_object() else { continue };
        let root = obj.get("root").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if root.is_empty() {
            continue;
        }
        let root = resolve_path(&root);
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| basename(&root));
        let last_opened_at = obj.get("lastOpenedAt").and_then(Value::as_i64).unwrap_or(0);
        // De-dup by root, keeping the most recent.
        if let Some(existing) = out.iter_mut().find(|e| e.root == root) {
            if last_opened_at > existing.last_opened_at {
                existing.last_opened_at = last_opened_at;
                existing.name = name;
            }
        } else {
            out.push(PersistedLocalVault { root, name, last_opened_at });
        }
    }
    out.sort_by(|a, b| {
        b.last_opened_at
            .cmp(&a.last_opened_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Port of `rememberLocalVault` — most-recent-first, de-duped, capped at 20.
pub fn remember_local_vault(
    entries: &[PersistedLocalVault],
    vault: &VaultInfo,
    opened_at: i64,
) -> Vec<PersistedLocalVault> {
    let root = resolve_path(&vault.root);
    let next = PersistedLocalVault {
        root: root.clone(),
        name: if vault.name.is_empty() { basename(&root) } else { vault.name.clone() },
        last_opened_at: opened_at,
    };
    let mut rest: Vec<PersistedLocalVault> = entries
        .iter()
        .filter(|e| resolve_path(&e.root) != root)
        .cloned()
        .collect();
    rest.sort_by(|a, b| {
        b.last_opened_at
            .cmp(&a.last_opened_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut out = vec![next];
    out.extend(rest);
    out.truncate(MAX_REMEMBERED_VAULTS);
    out
}

/// Port of `forgetLocalVault`.
pub fn forget_local_vault(
    entries: &[PersistedLocalVault],
    root: &str,
) -> Vec<PersistedLocalVault> {
    let target = resolve_path(root);
    entries
        .iter()
        .filter(|e| resolve_path(&e.root) != target)
        .cloned()
        .collect()
}

pub fn local_vaults_to_entries(vaults: &[PersistedLocalVault]) -> Vec<LocalVaultEntry> {
    vaults
        .iter()
        .map(|v| LocalVaultEntry {
            root: v.root.clone(),
            name: v.name.clone(),
            last_opened_at: v.last_opened_at,
        })
        .collect()
}

// ---- IO ------------------------------------------------------------------

fn read_config_file(path: &Path) -> Option<PersistedConfig> {
    let raw = fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&raw).ok()?;
    Some(PersistedConfig::from_value(&value))
}

/// Load the config, falling back to the `.bak` sibling on a corrupt/missing
/// primary, and finally to defaults.
pub fn load_config(config_dir: &Path) -> PersistedConfig {
    let primary = config_dir.join(CONFIG_FILE);
    let backup = backup_path(config_dir);
    if let Some(cfg) = read_config_file(&primary) {
        return cfg;
    }
    if let Some(cfg) = read_config_file(&backup) {
        return cfg;
    }
    PersistedConfig::default()
}

/// Atomically write the config: refresh the `.bak`, write a temp file, rename.
pub fn save_config(config_dir: &Path, cfg: &PersistedConfig) -> std::io::Result<()> {
    fs::create_dir_all(config_dir)?;
    let target = config_dir.join(CONFIG_FILE);
    let backup = backup_path(config_dir);
    if target.exists() {
        let _ = fs::copy(&target, &backup);
    }
    let body = serde_json::to_string_pretty(&cfg.to_value()).unwrap_or_else(|_| "{}".into());
    let tmp = config_dir.join(format!("{}.{}.tmp", CONFIG_FILE, process::id()));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &target)?;
    Ok(())
}

/// Load → mutate → save.
pub fn update_config<F>(config_dir: &Path, updater: F) -> std::io::Result<PersistedConfig>
where
    F: FnOnce(&mut PersistedConfig),
{
    let mut cfg = load_config(config_dir);
    updater(&mut cfg);
    save_config(config_dir, &cfg)?;
    Ok(cfg)
}

fn backup_path(config_dir: &Path) -> PathBuf {
    config_dir.join(format!("{CONFIG_FILE}.bak"))
}

// ---- path helpers --------------------------------------------------------

pub fn resolve_path(p: &str) -> String {
    let path = Path::new(p);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    // Lexical normalization (don't touch the filesystem; the dir may not exist).
    normalize_lexical(&abs).to_string_lossy().to_string()
}

fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(root: &str) -> VaultInfo {
        VaultInfo { root: root.into(), name: basename(&resolve_path(root)) }
    }

    #[test]
    fn remember_dedups_and_orders_most_recent_first() {
        let entries = vec![];
        let entries = remember_local_vault(&entries, &vault("/tmp/a"), 100);
        let entries = remember_local_vault(&entries, &vault("/tmp/b"), 200);
        // Re-opening /tmp/a bumps it to the front without duplicating.
        let entries = remember_local_vault(&entries, &vault("/tmp/a"), 300);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].root.ends_with("/tmp/a"));
        assert!(entries[1].root.ends_with("/tmp/b"));
    }

    #[test]
    fn remember_caps_at_twenty() {
        let mut entries = vec![];
        for i in 0..25 {
            entries = remember_local_vault(&entries, &vault(&format!("/tmp/v{i}")), i as i64);
        }
        assert_eq!(entries.len(), MAX_REMEMBERED_VAULTS);
    }

    #[test]
    fn forget_removes_matching_root() {
        let entries = remember_local_vault(&[], &vault("/tmp/a"), 1);
        let entries = remember_local_vault(&entries, &vault("/tmp/b"), 2);
        let after = forget_local_vault(&entries, "/tmp/a");
        assert_eq!(after.len(), 1);
        assert!(after[0].root.ends_with("/tmp/b"));
    }

    #[test]
    fn save_load_roundtrips_and_preserves_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = PersistedConfig::default();
        cfg.vault_root = Some("/tmp/vault".into());
        cfg.local_vaults = remember_local_vault(&[], &vault("/tmp/vault"), 42);
        // A field owned by a later milestone must survive a round-trip.
        cfg.extra.insert("remoteWorkspaceProfiles".into(), serde_json::json!([{ "id": "x" }]));
        save_config(dir.path(), &cfg).unwrap();

        let loaded = load_config(dir.path());
        assert_eq!(loaded.vault_root.as_deref(), Some("/tmp/vault"));
        assert_eq!(loaded.local_vaults.len(), 1);
        assert_eq!(
            loaded.extra.get("remoteWorkspaceProfiles"),
            Some(&serde_json::json!([{ "id": "x" }]))
        );
    }

    #[test]
    fn corrupt_primary_falls_back_to_backup() {
        let dir = tempfile::tempdir().unwrap();
        // Write a good config first (creates the primary), then again (rotates
        // the good one into `.bak`), then corrupt the primary.
        let mut cfg = PersistedConfig::default();
        cfg.vault_root = Some("/tmp/good".into());
        save_config(dir.path(), &cfg).unwrap();
        save_config(dir.path(), &cfg).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "{ not json").unwrap();

        let loaded = load_config(dir.path());
        assert_eq!(loaded.vault_root.as_deref(), Some("/tmp/good"));
    }
}
