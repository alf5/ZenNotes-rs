//! The seeded inbox, settled: port of `settleSeededInbox`, `removeSeedOnlyInbox`
//! and `takeAwayUntouchedWelcome` from apps/desktop/src/main/vault.ts (v2.84).
//!
//! A folder opened empty is seeded in inbox mode without saying so in
//! vault.json, so the layout is still inferred when folders are copied in,
//! and it turns to root mode. That is right, it shows what was copied. What it
//! must not leave behind is the system Inbox as a lowercase `inbox` folder
//! holding a Welcome nobody asked for. So `inbox/` goes when it only holds what
//! the app put there; anything else in it stays, a note you edited included.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::layout::{INTERNAL_VAULT_DIR, VAULT_SETTINGS_FILE};

/// SHA-256 of every Welcome note seeded into a vault this app may open. None
/// ever leaves: these bytes are the proof a vault's Welcome is still a seed
/// and nobody wrote in it. The port's own Welcome texts come first (a test
/// holds the current one to this list); upstream's follow, because a vault
/// upstream ZenNotes seeded opens here too.
const WELCOME_SEED_SHA256: [&str; 6] = [
    // ZenNotes-rs, since the rename (f9bd92c).
    "48f209ace74f6a686faa6320da1b35206a5faacd41dfc7ff5435fe2ed56ee025",
    // SynNotes, the port's first name (c048e89).
    "456a0019453e18937e31efc73f4a171623ec4c0ef3b71763fc7205e3266d50fc",
    // Upstream's WELCOME_SEED_SHA256, v2.85.0.
    "50c264c87b2790dbb685cd9a699118e1d62e84eb5532701e96d5b1ab55373fac",
    "a5d1b711723c817947c48b844ddc789b9889ce13645a060f33e6e6c4362cf1af",
    "f1d62f8864688c0c51d627c1156c8a00b0c3925eddf0678dfe3da84dd5e6a9e8",
    "04fabc332abe4b892d548782ecfcec27b77cf157ec59c7f0663b5774fae01aa9",
];

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn is_seed(path: &Path) -> std::io::Result<bool> {
    let hash = sha256_file(path)?;
    Ok(WELCOME_SEED_SHA256.contains(&hash.as_str()))
}

/// The inbox directory vault.json names. The port does not remap system
/// folders (#115), so a vault that does belongs to upstream's rules: it keeps
/// its inbox, whatever it is called.
pub fn inbox_is_remapped(parsed: &Value) -> bool {
    parsed
        .get("systemFolderPaths")
        .and_then(|paths| paths.get("inbox"))
        .and_then(Value::as_str)
        .is_some_and(|dir| dir != "inbox")
}

/// Port of `settleSeededInbox`. True when `inbox/` was taken away.
pub fn settle_seeded_inbox(root: &Path) -> bool {
    let settings = root.join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE);
    let parsed = match fs::read_to_string(&settings) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            // A vault.json that does not parse states nothing we can read, and
            // leaves its owner a problem that is not this function's to act on.
            Err(_) => return false,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(_) => return false,
    };
    // A vault that says root has no Inbox to keep. One that says inbox keeps
    // it, however much is at its root: that is what the banner is for.
    match parsed.get("primaryNotesLocation") {
        Some(Value::Null) | None => {}
        Some(Value::String(mode)) if mode == "root" => {}
        Some(_) => return false,
    }
    if inbox_is_remapped(&parsed) {
        return false;
    }
    if super::settings::infer_primary_notes_location(root) != "root" {
        return false;
    }
    remove_seed_only_inbox(root, false)
}

/// Port of `removeSeedOnlyInbox`. `explicit` is a switch to root mode the
/// person asked for: only then does an empty `inbox/` go, since without a
/// Welcome there is no sign this app made it.
pub fn remove_seed_only_inbox(root: &Path, explicit: bool) -> bool {
    try_remove_seed_only_inbox(root, explicit).unwrap_or(false)
}

fn try_remove_seed_only_inbox(root: &Path, explicit: bool) -> std::io::Result<bool> {
    // `Inbox/` is theirs by name, and a case-insensitive volume would answer
    // for it if this looked the folder up by path.
    let has_inbox = fs::read_dir(root)?
        .flatten()
        .any(|entry| entry.file_name() == "inbox" && entry.file_type().is_ok_and(|t| t.is_dir()));
    if !has_inbox {
        return Ok(false);
    }
    let inbox = root.join("inbox");
    // Finder writes .DS_Store into any folder it shows; it says nothing about
    // the folder.
    let inside: Vec<fs::DirEntry> = fs::read_dir(&inbox)?
        .flatten()
        .filter(|entry| entry.file_name() != ".DS_Store")
        .collect();
    if inside.len() > 1 {
        return Ok(false);
    }
    match inside.first() {
        None if !explicit => return Ok(false),
        None => {}
        Some(only) => {
            if only.file_name() != "Welcome.md" || !only.file_type()?.is_file() {
                return Ok(false);
            }
            if super::settings::infer_primary_notes_location(root) != "root" {
                return Ok(false);
            }
            if super::comments::comments_path(root, "inbox/Welcome.md").symlink_metadata().is_ok() {
                return Ok(false);
            }
            if !take_away_untouched_welcome(&inbox)? {
                return Ok(false);
            }
            let _ = super::sidecars::remove_note_creation(root, "inbox/Welcome.md", false);
        }
    }
    let _ = fs::remove_file(inbox.join(".DS_Store"));
    // Not recursive: a file landing after the look stays, and so does the folder.
    fs::remove_dir(&inbox)?;
    Ok(true)
}

/// Port of `takeAwayUntouchedWelcome`. It is hashed where nothing can save to
/// it any more: an edit landing between a hash and a delete would be lost. A
/// save after the rename makes a new file, which keeps the folder. An edit the
/// rename carried with it goes back, beside any newer file rather than over it.
fn take_away_untouched_welcome(inbox: &Path) -> std::io::Result<bool> {
    let welcome = inbox.join("Welcome.md");
    if !is_seed(&welcome)? {
        return Ok(false);
    }
    let aside = inbox.join(format!(".Welcome.settling-{}", uuid::Uuid::new_v4()));
    fs::rename(&welcome, &aside)?;
    if is_seed(&aside)? {
        fs::remove_file(&aside)?;
        return Ok(true);
    }
    let back = if welcome.symlink_metadata().is_ok() {
        inbox.join(format!("Welcome {}.md", uuid::Uuid::new_v4()))
    } else {
        welcome
    };
    fs::rename(&aside, back)?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::layout::ensure_vault_layout;

    fn seeded() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        ensure_vault_layout(dir.path()).unwrap();
        assert!(dir.path().join("inbox/Welcome.md").is_file());
        dir
    }

    #[test]
    fn the_welcome_the_port_seeds_is_a_known_seed() {
        let dir = seeded();
        assert!(is_seed(&dir.path().join("inbox/Welcome.md")).unwrap());
    }

    #[test]
    fn copied_in_folders_take_the_seeded_inbox_away() {
        let dir = seeded();
        fs::create_dir_all(dir.path().join("Projects")).unwrap();
        fs::write(dir.path().join("Projects/Plan.md"), "# Plan").unwrap();
        assert!(settle_seeded_inbox(dir.path()));
        assert!(!dir.path().join("inbox").exists());
    }

    #[test]
    fn an_edited_welcome_keeps_the_inbox() {
        let dir = seeded();
        fs::write(dir.path().join("inbox/Welcome.md"), "my own words").unwrap();
        fs::write(dir.path().join("Plan.md"), "# Plan").unwrap();
        assert!(!settle_seeded_inbox(dir.path()));
        assert_eq!(fs::read_to_string(dir.path().join("inbox/Welcome.md")).unwrap(), "my own words");
    }

    #[test]
    fn a_second_note_or_a_comment_keeps_the_inbox() {
        let dir = seeded();
        fs::write(dir.path().join("Plan.md"), "# Plan").unwrap();
        fs::write(dir.path().join("inbox/Idea.md"), "idea").unwrap();
        assert!(!settle_seeded_inbox(dir.path()));
        fs::remove_file(dir.path().join("inbox/Idea.md")).unwrap();
        let comments = crate::vault::comments::comments_path(dir.path(), "inbox/Welcome.md");
        fs::create_dir_all(comments.parent().unwrap()).unwrap();
        fs::write(&comments, "{}").unwrap();
        assert!(!settle_seeded_inbox(dir.path()));
        assert!(dir.path().join("inbox/Welcome.md").is_file());
    }

    #[test]
    fn a_vault_that_states_inbox_mode_keeps_its_inbox() {
        let dir = seeded();
        fs::write(dir.path().join("Plan.md"), "# Plan").unwrap();
        fs::create_dir_all(dir.path().join(".zennotes")).unwrap();
        fs::write(dir.path().join(".zennotes/vault.json"), r#"{"primaryNotesLocation":"inbox"}"#).unwrap();
        assert!(!settle_seeded_inbox(dir.path()));
        fs::write(dir.path().join(".zennotes/vault.json"), "{ not json").unwrap();
        assert!(!settle_seeded_inbox(dir.path()));
        assert!(dir.path().join("inbox/Welcome.md").is_file());
    }

    #[test]
    fn nothing_beside_the_inbox_keeps_it() {
        let dir = seeded();
        assert!(!settle_seeded_inbox(dir.path()));
        assert!(dir.path().join("inbox/Welcome.md").is_file());
    }

    #[test]
    fn an_empty_inbox_goes_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("inbox")).unwrap();
        fs::write(dir.path().join("Plan.md"), "# Plan").unwrap();
        assert!(!remove_seed_only_inbox(dir.path(), false));
        assert!(remove_seed_only_inbox(dir.path(), true));
        assert!(!dir.path().join("inbox").exists());
    }
}
