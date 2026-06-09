//! Secret storage for remote-workspace auth tokens — port of
//! apps/desktop/src/main/secret-store.ts (keytar → the `keyring` crate, with an
//! encrypted-file fallback when no OS keychain backend is available).
//!
//! Note: the rebrand changes the keychain service name, so tokens do not
//! migrate from a ZenNotes install (parity risk #3).

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine;

const SERVICE: &str = "com.synnotes.app";
const ACCOUNT_PREFIX: &str = "remote-workspace:";

fn account(id: &str) -> String {
    format!("{ACCOUNT_PREFIX}{id}")
}

fn fallback_path(config_dir: &PathBuf) -> PathBuf {
    config_dir.join("remote-workspace-secrets.json")
}

fn read_fallback(config_dir: &PathBuf) -> HashMap<String, String> {
    std::fs::read_to_string(fallback_path(config_dir))
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
        .unwrap_or_default()
}

fn write_fallback(config_dir: &PathBuf, map: &HashMap<String, String>) {
    if let Ok(body) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(fallback_path(config_dir), body);
    }
}

/// Lightly obfuscate fallback secrets (base64). Not real encryption — the OS
/// keychain is the real store; this only avoids plaintext on disk when no
/// keychain backend exists.
fn obfuscate(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}
fn deobfuscate(s: &str) -> Option<String> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

pub fn get_secret(config_dir: &PathBuf, id: &str) -> Option<String> {
    if let Ok(entry) = keyring::Entry::new(SERVICE, &account(id)) {
        if let Ok(pw) = entry.get_password() {
            return Some(pw);
        }
    }
    read_fallback(config_dir).get(id).and_then(|v| deobfuscate(v))
}

pub fn set_secret(config_dir: &PathBuf, id: &str, secret: Option<&str>) {
    match secret {
        Some(value) => {
            let stored = keyring::Entry::new(SERVICE, &account(id))
                .and_then(|e| e.set_password(value))
                .is_ok();
            if !stored {
                let mut map = read_fallback(config_dir);
                map.insert(id.to_string(), obfuscate(value));
                write_fallback(config_dir, &map);
            }
        }
        None => delete_secret(config_dir, id),
    }
}

pub fn delete_secret(config_dir: &PathBuf, id: &str) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, &account(id)) {
        let _ = entry.delete_credential();
    }
    let mut map = read_fallback(config_dir);
    if map.remove(id).is_some() {
        write_fallback(config_dir, &map);
    }
}

pub fn has_secret(config_dir: &PathBuf, id: &str) -> bool {
    get_secret(config_dir, id).is_some()
}
