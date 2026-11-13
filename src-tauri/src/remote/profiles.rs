//! Saved remote workspaces: port of the profile half of upstream's
//! apps/desktop/src/main/index.ts (`listRemoteWorkspaceProfiles`,
//! `saveRemoteWorkspaceProfile`, `deleteRemoteWorkspaceProfile`,
//! `deriveRemoteWorkspaceProfileName`).
//!
//! Profiles live in `zennotes-rs.config.json` under the keys upstream uses
//! (`remoteWorkspaceProfiles`, `remoteWorkspaceProfileId`, `remoteWorkspace`).
//! Tokens never do: they go to the OS keychain through `secrets`, keyed by
//! profile id. One deliberate difference: an edit keeps the profile's
//! `lastConnectedAt`, where upstream resets it to null.

use std::path::Path;

use serde_json::{json, Value};

use crate::ipc::types::{RemoteWorkspaceProfile, RemoteWorkspaceProfileInput};
use crate::secrets;
use crate::vault::config::{self, PersistedConfig};

pub const PROFILES_KEY: &str = "remoteWorkspaceProfiles";
pub const CURRENT_PROFILE_KEY: &str = "remoteWorkspaceProfileId";
/// `{ baseUrl }` of the live connection (upstream's `remoteWorkspace`).
pub const CONNECTION_KEY: &str = "remoteWorkspace";

/// A saved profile as the config file holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub vault_path: Option<String>,
    pub last_connected_at: Option<i64>,
}

impl StoredProfile {
    fn from_value(raw: &Value) -> Option<Self> {
        let obj = raw.as_object()?;
        let id = obj.get("id").and_then(Value::as_str)?.trim().to_string();
        if id.is_empty() {
            return None;
        }
        Some(StoredProfile {
            id,
            name: obj.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            base_url: obj.get("baseUrl").and_then(Value::as_str).unwrap_or("").to_string(),
            vault_path: obj
                .get("vaultPath")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string),
            last_connected_at: obj.get("lastConnectedAt").and_then(Value::as_i64),
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "baseUrl": self.base_url,
            "vaultPath": self.vault_path,
            "lastConnectedAt": self.last_connected_at,
        })
    }
}

/// Upstream's `normalizeRemoteBaseUrl`: trimmed, no trailing slash. The
/// scheme is left as typed; the connection adds one (see `session`).
pub fn normalize_profile_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

pub fn stored_profiles(cfg: &PersistedConfig) -> Vec<StoredProfile> {
    cfg.extra
        .get(PROFILES_KEY)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(StoredProfile::from_value).collect())
        .unwrap_or_default()
}

fn set_stored_profiles(cfg: &mut PersistedConfig, profiles: &[StoredProfile]) {
    cfg.extra.insert(
        PROFILES_KEY.into(),
        Value::Array(profiles.iter().map(StoredProfile::to_value).collect()),
    );
}

pub fn current_profile_id(cfg: &PersistedConfig) -> Option<String> {
    cfg.extra
        .get(CURRENT_PROFILE_KEY)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub fn set_current_profile_id(cfg: &mut PersistedConfig, id: Option<&str>) {
    cfg.extra.insert(
        CURRENT_PROFILE_KEY.into(),
        id.map(|id| Value::String(id.to_string())).unwrap_or(Value::Null),
    );
}

/// The live connection's server URL, when one is configured.
pub fn connection_base_url(cfg: &PersistedConfig) -> Option<String> {
    cfg.extra
        .get(CONNECTION_KEY)
        .and_then(|v| v.get("baseUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

pub fn set_connection_base_url(cfg: &mut PersistedConfig, base_url: Option<&str>) {
    cfg.extra.insert(
        CONNECTION_KEY.into(),
        base_url.map(|url| json!({ "baseUrl": url })).unwrap_or(Value::Null),
    );
}

pub fn find_profile(cfg: &PersistedConfig, id: Option<&str>) -> Option<StoredProfile> {
    let id = id?;
    stored_profiles(cfg).into_iter().find(|p| p.id == id)
}

fn to_profile(config_dir: &Path, stored: &StoredProfile) -> RemoteWorkspaceProfile {
    RemoteWorkspaceProfile {
        id: stored.id.clone(),
        name: stored.name.clone(),
        base_url: stored.base_url.clone(),
        has_credential: secrets::has_saved_secret(config_dir, &stored.id),
        vault_path: stored.vault_path.clone(),
        last_connected_at: stored.last_connected_at,
    }
}

pub fn list_profiles(config_dir: &Path) -> Vec<RemoteWorkspaceProfile> {
    stored_profiles(&config::load_config(config_dir))
        .iter()
        .map(|p| to_profile(config_dir, p))
        .collect()
}

/// Upstream's `deriveRemoteWorkspaceProfileName`: `<vault> (<host>)`, or the
/// host alone, with a number added until no other profile has the name.
pub fn derive_profile_name(
    id: &str,
    base_url: &str,
    vault_path: Option<&str>,
    existing: &[StoredProfile],
) -> String {
    let normalized = normalize_profile_base_url(base_url);
    let mut host = "ZenNotes Server".to_string();
    let with_scheme = if has_http_scheme(&normalized) {
        normalized.clone()
    } else {
        format!("http://{normalized}")
    };
    match url::Url::parse(&with_scheme) {
        Ok(parsed) => {
            if let Some(h) = parsed.host_str() {
                host = match parsed.port() {
                    Some(port) => format!("{h}:{port}"),
                    None => h.to_string(),
                };
            }
        }
        Err(_) => {
            if !normalized.is_empty() {
                host = normalized.clone();
            }
        }
    }

    let mut base_name = host.clone();
    if let Some(path) = vault_path.map(str::trim).filter(|p| !p.is_empty()) {
        let path = path.replace('\\', "/");
        let path = path.trim_end_matches('/');
        let vault_name = path.rsplit('/').next().unwrap_or("");
        if !vault_name.is_empty() && vault_name != "." {
            base_name = format!("{vault_name} ({host})");
        }
    }

    let others: Vec<&StoredProfile> = existing.iter().filter(|p| p.id != id).collect();
    if !others.iter().any(|p| p.name == base_name) {
        return base_name;
    }
    let mut suffix = 2;
    while others.iter().any(|p| p.name == format!("{base_name} {suffix}")) {
        suffix += 1;
    }
    format!("{base_name} {suffix}")
}

pub fn has_http_scheme(value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The live connection a saved profile is checked against: its server URL and
/// the vault root it is on.
pub struct LiveConnection<'a> {
    pub base_url: &'a str,
    pub vault_root: Option<&'a str>,
}

fn profile_matches_connection(profile: &StoredProfile, live: &LiveConnection<'_>) -> bool {
    normalize_profile_base_url(&profile.base_url) == normalize_profile_base_url(live.base_url)
        && profile.vault_path.as_deref() == live.vault_root
}

/// `saveRemoteWorkspaceProfile`. `live` is the current connection, when one
/// exists: a profile that describes it becomes the current profile.
pub fn save_profile(
    config_dir: &Path,
    input: &RemoteWorkspaceProfileInput,
    live: Option<LiveConnection<'_>>,
) -> Result<RemoteWorkspaceProfile, String> {
    let id = input
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let base_url = normalize_profile_base_url(&input.base_url);
    if base_url.is_empty() {
        return Err("Remote workspace profiles need a server URL.".into());
    }
    let vault_path = input
        .vault_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let typed_name = input.name.as_deref().map(str::trim).unwrap_or("").to_string();

    config::update_config(config_dir, |cfg| {
        let profiles = stored_profiles(cfg);
        let previous = profiles.iter().find(|p| p.id == id).cloned();
        let name = if typed_name.is_empty() {
            derive_profile_name(&id, &base_url, vault_path.as_deref(), &profiles)
        } else {
            typed_name.clone()
        };
        let next = StoredProfile {
            id: id.clone(),
            name,
            base_url: base_url.clone(),
            vault_path: vault_path.clone(),
            last_connected_at: previous.and_then(|p| p.last_connected_at),
        };
        let mut next_profiles: Vec<StoredProfile> =
            profiles.into_iter().filter(|p| p.id != id).collect();
        next_profiles.push(next.clone());
        next_profiles.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        set_stored_profiles(cfg, &next_profiles);

        if let Some(live) = &live {
            if profile_matches_connection(&next, live) {
                set_current_profile_id(cfg, Some(&id));
            } else if current_profile_id(cfg).as_deref() == Some(id.as_str()) {
                set_current_profile_id(cfg, None);
            }
        }
    })
    .map_err(|e| format!("Remote workspace profile could not be saved: {e}"))?;

    // Tokens go to the keychain, never to the config file.
    // Without a keychain the token is kept for this run only, and the profile
    // says it has none saved.
    let typed_token = input.auth_token.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let mut saved_now = None;
    if input.clear_auth_token.unwrap_or(false) {
        secrets::delete_secret(config_dir, &id);
        saved_now = Some(false);
    } else if let Some(token) = typed_token {
        saved_now = Some(secrets::set_secret(config_dir, &id, token));
    }

    let cfg = config::load_config(config_dir);
    let stored = find_profile(&cfg, Some(&id))
        .ok_or_else(|| "Remote workspace profile could not be saved.".to_string())?;
    let mut profile = to_profile(config_dir, &stored);
    if let Some(saved) = saved_now {
        profile.has_credential = saved;
    }
    Ok(profile)
}

/// What deleting a profile did, for the session to act on.
pub struct DeletedProfile {
    /// The deleted profile had a saved token and was the current one, so the
    /// live connection loses its credential (upstream drops its client).
    pub dropped_live_credential: bool,
}

/// `deleteRemoteWorkspaceProfile`.
pub fn delete_profile(config_dir: &Path, id: &str) -> Result<DeletedProfile, String> {
    let had_secret = secrets::has_secret(config_dir, id);
    let mut was_current = false;
    config::update_config(config_dir, |cfg| {
        let profiles = stored_profiles(cfg);
        let deleted = profiles.iter().find(|p| p.id == id).cloned();
        let next: Vec<StoredProfile> = profiles.into_iter().filter(|p| p.id != id).collect();
        if let Some(deleted) = &deleted {
            let url = normalize_profile_base_url(&deleted.base_url);
            let others_share_url = next
                .iter()
                .any(|p| normalize_profile_base_url(&p.base_url) == url);
            let live_is_this_url = connection_base_url(cfg)
                .map(|live| normalize_profile_base_url(&live) == url)
                .unwrap_or(false);
            if live_is_this_url && !others_share_url {
                set_connection_base_url(cfg, None);
            }
        }
        if current_profile_id(cfg).as_deref() == Some(id) {
            was_current = true;
            set_current_profile_id(cfg, None);
        }
        set_stored_profiles(cfg, &next);
    })
    .map_err(|e| format!("Remote workspace profile could not be deleted: {e}"))?;
    secrets::delete_secret(config_dir, id);
    Ok(DeletedProfile { dropped_live_credential: had_secret && was_current })
}

/// Stamp a successful connection on the profile and make it the current one.
pub fn mark_connected(config_dir: &Path, id: &str, at: i64) -> Result<(), String> {
    config::update_config(config_dir, |cfg| {
        let mut profiles = stored_profiles(cfg);
        for p in profiles.iter_mut().filter(|p| p.id == id) {
            p.last_connected_at = Some(at);
        }
        set_stored_profiles(cfg, &profiles);
        set_current_profile_id(cfg, Some(id));
    })
    .map(|_| ())
    .map_err(|e| format!("Failed to persist config: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: Option<&str>, base_url: &str, vault_path: Option<&str>) -> RemoteWorkspaceProfileInput {
        RemoteWorkspaceProfileInput {
            id: None,
            name: name.map(str::to_string),
            base_url: base_url.into(),
            auth_token: None,
            clear_auth_token: None,
            vault_path: vault_path.map(str::to_string),
        }
    }

    #[test]
    fn save_list_delete_profiles() {
        let dir = tempfile::tempdir().unwrap();
        // No auth token → no keychain interaction in tests.
        let saved = save_profile(
            dir.path(),
            &input(Some("My Server"), "https://notes.example.com/", Some("/srv/vault")),
            None,
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
        let err = save_profile(dir.path(), &input(None, "  ", None), None).unwrap_err();
        assert_eq!(err, "Remote workspace profiles need a server URL.");
    }

    #[test]
    fn names_follow_upstream_and_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let a = save_profile(dir.path(), &input(None, "zen.example.com:7878", Some("/My vaults/Research")), None)
            .unwrap();
        assert_eq!(a.name, "Research (zen.example.com:7878)");
        let b = save_profile(dir.path(), &input(None, "https://zen.example.com:7878", Some("/x/Research/")), None)
            .unwrap();
        assert_eq!(b.name, "Research (zen.example.com:7878) 2");
        let c = save_profile(dir.path(), &input(None, "https://zen.example.com", None), None).unwrap();
        assert_eq!(c.name, "zen.example.com");
        // Sorted by name.
        let names: Vec<String> = list_profiles(dir.path()).into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["Research (zen.example.com:7878)", "Research (zen.example.com:7878) 2", "zen.example.com"]
        );
    }

    #[test]
    fn an_edit_keeps_last_connected_and_renaming_keeps_the_id() {
        let dir = tempfile::tempdir().unwrap();
        let saved = save_profile(dir.path(), &input(Some("A"), "https://a.example", None), None).unwrap();
        mark_connected(dir.path(), &saved.id, 42).unwrap();
        let mut edit = input(Some("B"), "https://a.example", None);
        edit.id = Some(saved.id.clone());
        let edited = save_profile(dir.path(), &edit, None).unwrap();
        assert_eq!(edited.id, saved.id);
        assert_eq!(edited.name, "B");
        assert_eq!(edited.last_connected_at, Some(42));
        let cfg = config::load_config(dir.path());
        assert_eq!(current_profile_id(&cfg).as_deref(), Some(saved.id.as_str()));
    }

    #[test]
    fn a_profile_describing_the_live_connection_becomes_current() {
        let dir = tempfile::tempdir().unwrap();
        let live = LiveConnection { base_url: "https://zen.example.com/", vault_root: Some("/My vaults/A") };
        let saved = save_profile(dir.path(), &input(None, "https://zen.example.com", Some("/My vaults/A")), Some(live))
            .unwrap();
        let cfg = config::load_config(dir.path());
        assert_eq!(current_profile_id(&cfg), Some(saved.id.clone()));

        // Re-pointing it at another vault while connected to A unsets it.
        let mut edit = input(None, "https://zen.example.com", Some("/My vaults/B"));
        edit.id = Some(saved.id.clone());
        let live = LiveConnection { base_url: "https://zen.example.com", vault_root: Some("/My vaults/A") };
        save_profile(dir.path(), &edit, Some(live)).unwrap();
        assert_eq!(current_profile_id(&config::load_config(dir.path())), None);
    }

    #[test]
    fn deleting_the_last_profile_on_a_server_forgets_the_connection_url() {
        let dir = tempfile::tempdir().unwrap();
        let a = save_profile(dir.path(), &input(Some("A"), "https://zen.example.com", Some("/a")), None).unwrap();
        let b = save_profile(dir.path(), &input(Some("B"), "https://zen.example.com/", Some("/b")), None).unwrap();
        config::update_config(dir.path(), |cfg| {
            set_connection_base_url(cfg, Some("https://zen.example.com"));
            set_current_profile_id(cfg, Some(&a.id));
        })
        .unwrap();

        delete_profile(dir.path(), &a.id).unwrap();
        let cfg = config::load_config(dir.path());
        assert_eq!(current_profile_id(&cfg), None);
        assert_eq!(connection_base_url(&cfg).as_deref(), Some("https://zen.example.com"));

        delete_profile(dir.path(), &b.id).unwrap();
        assert_eq!(connection_base_url(&config::load_config(dir.path())), None);
    }
}
