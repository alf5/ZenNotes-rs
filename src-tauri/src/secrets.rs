//! Secret storage for remote-workspace auth tokens: port of
//! apps/desktop/src/main/secret-store.ts (keytar/safeStorage → the `keyring`
//! crate).
//!
//! Tokens are kept in the OS keychain and nowhere else on disk. Like upstream,
//! which saves nothing when `safeStorage` is unavailable, a system without a
//! keychain keeps the token in memory for this run only, so the connection
//! works until the app quits and the next launch asks for it again.
//!
//! Builds before 2.79.3 wrote a base64 copy to `remote-workspace-secrets.json`
//! when the keychain refused. That file is still read, so nothing saved there is
//! lost, and each token in it moves into the keychain the first time the
//! keychain takes it. Nothing new is ever written to it.
//!
//! The rebrand changes the keychain service name, so tokens do not migrate from
//! a ZenNotes install (parity risk #3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;

const SERVICE: &str = "com.zennotes-rs.app";
const ACCOUNT_PREFIX: &str = "remote-workspace:";

/// Tokens the keychain would not take, kept for this run only.
static SESSION_ONLY: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn account(id: &str) -> String {
    format!("{ACCOUNT_PREFIX}{id}")
}

fn legacy_path(config_dir: &Path) -> PathBuf {
    config_dir.join("remote-workspace-secrets.json")
}

fn read_legacy(config_dir: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(legacy_path(config_dir))
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
        .unwrap_or_default()
}

/// Rewrite the legacy file with entries removed, or delete it once empty.
/// Only ever shrinks it.
fn write_legacy(config_dir: &Path, map: &HashMap<String, String>) {
    let path = legacy_path(config_dir);
    if map.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let Ok(body) = serde_json::to_string_pretty(map) else { return };
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if write_private(&tmp, body.as_bytes()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

fn decode_legacy(s: &str) -> Option<String> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

fn session_get(id: &str) -> Option<String> {
    SESSION_ONLY.lock().ok()?.as_ref()?.get(id).cloned()
}

fn session_set(id: &str, secret: Option<&str>) {
    if let Ok(mut guard) = SESSION_ONLY.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        match secret {
            Some(value) => {
                map.insert(id.to_string(), value.to_string());
            }
            None => {
                map.remove(id);
            }
        }
    }
}

fn keychain_store(id: &str, secret: &str) -> bool {
    keyring::Entry::new(SERVICE, &account(id))
        .and_then(|e| e.set_password(secret))
        .is_ok()
}

pub fn get_secret(config_dir: &Path, id: &str) -> Option<String> {
    if let Ok(entry) = keyring::Entry::new(SERVICE, &account(id)) {
        if let Ok(pw) = entry.get_password() {
            return Some(pw);
        }
    }
    let mut legacy = read_legacy(config_dir);
    if let Some(token) = legacy.get(id).and_then(|v| decode_legacy(v)) {
        if keychain_store(id, &token) {
            legacy.remove(id);
            write_legacy(config_dir, &legacy);
        }
        return Some(token);
    }
    session_get(id)
}

/// Save a token. Returns whether it reached the keychain; when it did not, it
/// is kept for this run only.
pub fn set_secret(config_dir: &Path, id: &str, secret: &str) -> bool {
    let stored = keychain_store(id, secret);
    if stored {
        session_set(id, None);
    } else {
        eprintln!(
            "[remote] no OS keychain took the token for remote workspace {id}; it is kept for this run only"
        );
        session_set(id, Some(secret));
    }
    let mut legacy = read_legacy(config_dir);
    if legacy.remove(id).is_some() {
        write_legacy(config_dir, &legacy);
    }
    stored
}

pub fn delete_secret(config_dir: &Path, id: &str) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, &account(id)) {
        let _ = entry.delete_credential();
    }
    session_set(id, None);
    let mut legacy = read_legacy(config_dir);
    if legacy.remove(id).is_some() {
        write_legacy(config_dir, &legacy);
    }
}

pub fn has_secret(config_dir: &Path, id: &str) -> bool {
    get_secret(config_dir, id).is_some()
}

/// A token kept between launches (keychain, or the legacy file), not one held
/// for this run only. The profile's "stored securely" note reads this.
pub fn has_saved_secret(config_dir: &Path, id: &str) -> bool {
    let in_keychain = keyring::Entry::new(SERVICE, &account(id))
        .and_then(|e| e.get_password())
        .is_ok();
    in_keychain || read_legacy(config_dir).contains_key(id)
}

/// Whether remote-workspace tokens can be kept between launches here.
pub fn remote_tokens_persist() -> bool {
    keychain_available(&account("probe"))
}

/// Keychain-only storage, for a secret that must never fall back to a file
/// (the Unsplash Access Key, whose terms keep it confidential). `account` is
/// the full keychain account name.
pub fn keychain_get(account: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, account).ok()?.get_password().ok()
}

pub fn keychain_set(account: &str, secret: &str) -> bool {
    keyring::Entry::new(SERVICE, account)
        .and_then(|e| e.set_password(secret))
        .is_ok()
}

pub fn keychain_delete(account: &str) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, account) {
        let _ = entry.delete_credential();
    }
}

/// Whether the OS keychain answers at all: a Linux desktop without a keyring
/// says no here, the way upstream's `safeStorage.isEncryptionAvailable` does.
pub fn keychain_available(account: &str) -> bool {
    match keyring::Entry::new(SERVICE, account).map(|e| e.get_password()) {
        Ok(Ok(_)) | Ok(Err(keyring::Error::NoEntry)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legacy_file_only_shrinks() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = HashMap::new();
        map.insert("a".to_string(), base64::engine::general_purpose::STANDARD.encode("tok-a"));
        map.insert("b".to_string(), base64::engine::general_purpose::STANDARD.encode("tok-b"));
        std::fs::write(legacy_path(dir.path()), serde_json::to_string(&map).unwrap()).unwrap();

        let mut legacy = read_legacy(dir.path());
        assert_eq!(legacy.get("a").and_then(|v| decode_legacy(v)).as_deref(), Some("tok-a"));
        legacy.remove("a");
        write_legacy(dir.path(), &legacy);
        let left = read_legacy(dir.path());
        assert_eq!(left.len(), 1);
        assert!(left.contains_key("b"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(legacy_path(dir.path())).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        write_legacy(dir.path(), &HashMap::new());
        assert!(!legacy_path(dir.path()).exists());
    }

    #[test]
    fn session_only_tokens_can_be_replaced_and_forgotten() {
        session_set("zz-test", Some("one"));
        assert_eq!(session_get("zz-test").as_deref(), Some("one"));
        session_set("zz-test", Some("two"));
        assert_eq!(session_get("zz-test").as_deref(), Some("two"));
        session_set("zz-test", None);
        assert_eq!(session_get("zz-test"), None);
    }
}
