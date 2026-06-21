//! Remote-workspace profile management — the persistence half of the remote
//! workspace feature (apps/desktop/src/main/index.ts remote-profile handlers).
//!
//! Profiles persist in `zennotes-rs.config.json` (`remoteWorkspaceProfiles`); auth
//! tokens live in the OS keychain via `secrets`. The live HTTP/WebSocket server
//! client (connect/disconnect/browse/select over the self-hosted Go server) is
//! NOT ported in this build — those commands return an unsupported error and
//! `supportsRemoteWorkspace` stays off (parity gap, documented).

use std::path::Path;

use serde_json::{json, Value};

use crate::ipc::types::{RemoteWorkspaceProfile, RemoteWorkspaceProfileInput};
use crate::secrets;
use crate::vault::config;

const PROFILES_KEY: &str = "remoteWorkspaceProfiles";

fn read_raw_profiles(config_dir: &Path) -> Vec<Value> {
    config::load_config(config_dir)
        .extra
        .get(PROFILES_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn write_raw_profiles(config_dir: &Path, profiles: Vec<Value>) -> Result<(), String> {
    config::update_config(config_dir, |cfg| {
        cfg.extra.insert(PROFILES_KEY.to_string(), Value::Array(profiles));
    })
    .map(|_| ())
    .map_err(|e| format!("persist failed: {e}"))
}

fn to_profile(config_dir: &Path, raw: &Value) -> Option<RemoteWorkspaceProfile> {
    let obj = raw.as_object()?;
    let id = obj.get("id").and_then(Value::as_str)?.to_string();
    let base_url = obj.get("baseUrl").and_then(Value::as_str).unwrap_or("").to_string();
    let name = obj.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    Some(RemoteWorkspaceProfile {
        has_credential: secrets::has_secret(&config_dir.to_path_buf(), &id),
        id,
        name,
        base_url,
        vault_path: obj.get("vaultPath").and_then(Value::as_str).map(str::to_string),
        last_connected_at: obj.get("lastConnectedAt").and_then(Value::as_i64),
    })
}

pub fn list_profiles(config_dir: &Path) -> Vec<RemoteWorkspaceProfile> {
    read_raw_profiles(config_dir)
        .iter()
        .filter_map(|raw| to_profile(config_dir, raw))
        .collect()
}

pub fn save_profile(
    config_dir: &Path,
    input: &RemoteWorkspaceProfileInput,
) -> Result<RemoteWorkspaceProfile, String> {
    let base_url = input.base_url.trim().to_string();
    if base_url.is_empty() {
        return Err("Server URL is required.".into());
    }
    let mut profiles = read_raw_profiles(config_dir);
    let id = input
        .id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let name = input
        .name
        .clone()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| base_url.clone());

    let entry = json!({
        "id": id,
        "name": name,
        "baseUrl": base_url,
        "vaultPath": input.vault_path,
        "lastConnectedAt": Value::Null,
    });
    if let Some(existing) = profiles.iter_mut().find(|p| p.get("id").and_then(Value::as_str) == Some(id.as_str())) {
        // Preserve lastConnectedAt across edits.
        let last = existing.get("lastConnectedAt").cloned().unwrap_or(Value::Null);
        *existing = entry;
        existing["lastConnectedAt"] = last;
    } else {
        profiles.push(entry);
    }
    write_raw_profiles(config_dir, profiles)?;

    // Token side-effects go to the keychain, never to the config file.
    let dir = config_dir.to_path_buf();
    if input.clear_auth_token.unwrap_or(false) {
        secrets::set_secret(&dir, &id, None);
    } else if let Some(token) = input.auth_token.as_deref().filter(|t| !t.is_empty()) {
        secrets::set_secret(&dir, &id, Some(token));
    }

    list_profiles(config_dir)
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "profile vanished after save".to_string())
}

pub fn delete_profile(config_dir: &Path, id: &str) -> Result<(), String> {
    let profiles: Vec<Value> = read_raw_profiles(config_dir)
        .into_iter()
        .filter(|p| p.get("id").and_then(Value::as_str) != Some(id))
        .collect();
    write_raw_profiles(config_dir, profiles)?;
    secrets::delete_secret(&config_dir.to_path_buf(), id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_delete_profiles() {
        let dir = tempfile::tempdir().unwrap();
        // No auth token → no keychain interaction in tests.
        let saved = save_profile(
            dir.path(),
            &RemoteWorkspaceProfileInput {
                id: None,
                name: Some("My Server".into()),
                base_url: "https://notes.example.com".into(),
                auth_token: None,
                clear_auth_token: None,
                vault_path: Some("/srv/vault".into()),
            },
        )
        .unwrap();
        assert_eq!(saved.name, "My Server");
        assert!(!saved.has_credential);

        let list = list_profiles(dir.path());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].base_url, "https://notes.example.com");
        assert_eq!(list[0].vault_path.as_deref(), Some("/srv/vault"));

        delete_profile(dir.path(), &saved.id).unwrap();
        assert!(list_profiles(dir.path()).is_empty());
    }

    #[test]
    fn save_requires_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_profile(
            dir.path(),
            &RemoteWorkspaceProfileInput {
                id: None,
                name: None,
                base_url: "  ".into(),
                auth_token: None,
                clear_auth_token: None,
                vault_path: None,
            },
        );
        assert!(err.is_err());
    }
}
