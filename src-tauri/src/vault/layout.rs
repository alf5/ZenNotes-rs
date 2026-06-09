//! Vault folder layout — port of `ensureVaultLayout`, `vaultInfo`,
//! `vaultLooksEmpty` from apps/desktop/src/main/vault.ts.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ipc::types::VaultInfo;

pub const FOLDERS: [&str; 4] = ["inbox", "quick", "archive", "trash"];
pub const INTERNAL_VAULT_DIR: &str = ".zennotes";
pub const VAULT_SETTINGS_FILE: &str = "vault.json";
pub const PRIMARY_ATTACHMENTS_DIR: &str = "attachements";
pub const LEGACY_ATTACHMENTS_DIRS: [&str; 1] = ["_assets"];

pub fn is_system_folder(name: &str) -> bool {
    FOLDERS.contains(&name)
}

/// Names that may not be top-level note folders (system folders + attachment
/// dirs + the internal `.zennotes` dir). Mirrors `RESERVED_ROOT_NAMES`.
pub fn is_reserved_root_name(name: &str) -> bool {
    is_system_folder(name)
        || name == PRIMARY_ATTACHMENTS_DIR
        || LEGACY_ATTACHMENTS_DIRS.contains(&name)
        || name == INTERNAL_VAULT_DIR
}

/// Entries hidden when the vault root itself is the primary notes folder
/// (root mode). Mirrors `HIDDEN_PRIMARY_ROOT_NAMES`.
pub fn should_hide_primary_root_entry(name: &str) -> bool {
    name == "quick"
        || name == "archive"
        || name == "trash"
        || name == PRIMARY_ATTACHMENTS_DIR
        || LEGACY_ATTACHMENTS_DIRS.contains(&name)
        || name == INTERNAL_VAULT_DIR
}

/// Absolute path of a top-level folder. `inbox` resolves to the primary notes
/// root (the vault root in root mode, `inbox/` otherwise).
pub fn folder_root(root: &Path, folder: &str) -> PathBuf {
    if folder == "inbox" {
        primary_notes_root(root)
    } else {
        root.join(folder)
    }
}

/// First-run welcome note (rebranded for SynNotes).
const WELCOME_NOTE: &str = r#"# Welcome to SynNotes

SynNotes is a **file-based** markdown notes app made for focus and deep work. Every note is a plain `.md` file in your vault — yours to keep, sync, and version however you like.

## What you get

- **GitHub-flavored markdown** — tables, task lists, footnotes, strikethrough
- **Wiki links** — jump between notes with [[double brackets]]
- **Tags** — write a hashtag like `#project` in any note and it appears in the sidebar
- **Math** — inline like $e^{i\pi}+1=0$ or as blocks
- **Callouts** — Obsidian-style `> [!note]` blocks
- **Mermaid diagrams** — code-fenced ```mermaid blocks render inline
- **Full-text search** — press `Space s t` in Vim mode, or run **Search Text in Vault** from the command palette

## Try it

- [ ] Write your first note
- [ ] Link to [[another note]]

> [!tip]
> Press the + button in the sidebar to create a new note. Your changes save automatically.

```js
// Syntax-highlighted code blocks just work
function hello(name) {
  return `Hello, ${name}!`
}
```

Enjoy the quiet.
"#;

pub fn vault_info(root: &Path) -> VaultInfo {
    VaultInfo {
        root: root.to_string_lossy().to_string(),
        name: root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    }
}

fn vault_settings_path(root: &Path) -> PathBuf {
    root.join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE)
}

/// Returns `"root"` or `"inbox"` (the default). Full vault-settings handling
/// lands in M10; this focused reader is all `ensureVaultLayout` needs.
pub fn read_primary_notes_location(root: &Path) -> String {
    if let Ok(raw) = fs::read_to_string(vault_settings_path(root)) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if value.get("primaryNotesLocation").and_then(|v| v.as_str()) == Some("root") {
                return "root".into();
            }
        }
    }
    "inbox".into()
}

/// The vault root, where primary notes live (`inbox/` or the root itself).
pub fn primary_notes_root(root: &Path) -> PathBuf {
    if read_primary_notes_location(root) == "root" {
        root.to_path_buf()
    } else {
        root.join("inbox")
    }
}

fn vault_looks_empty(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == INTERNAL_VAULT_DIR {
            continue;
        }
        return false;
    }
    true
}

/// Ensure the expected folder layout exists and seed a welcome note the very
/// first time a brand-new (empty) vault is opened.
pub fn ensure_vault_layout(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    let was_empty = vault_looks_empty(root);
    let primary_at_root = read_primary_notes_location(root) == "root";
    for folder in FOLDERS {
        if folder == "inbox" && primary_at_root {
            continue;
        }
        fs::create_dir_all(root.join(folder))?;
    }
    if was_empty {
        let welcome_dir = primary_notes_root(root);
        fs::create_dir_all(&welcome_dir)?;
        let welcome_path = welcome_dir.join("Welcome.md");
        if !welcome_path.exists() {
            fs::write(&welcome_path, WELCOME_NOTE)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_vault_gets_layout_and_welcome_note() {
        let dir = tempfile::tempdir().unwrap();
        ensure_vault_layout(dir.path()).unwrap();
        for folder in FOLDERS {
            assert!(dir.path().join(folder).is_dir(), "{folder} should exist");
        }
        assert!(dir.path().join("inbox/Welcome.md").is_file());
    }

    #[test]
    fn primary_root_mode_skips_inbox_and_seeds_at_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(INTERNAL_VAULT_DIR)).unwrap();
        fs::write(
            dir.path().join(INTERNAL_VAULT_DIR).join(VAULT_SETTINGS_FILE),
            r#"{"primaryNotesLocation":"root"}"#,
        )
        .unwrap();
        ensure_vault_layout(dir.path()).unwrap();
        assert!(!dir.path().join("inbox").exists());
        assert!(dir.path().join("Welcome.md").is_file());
    }

    #[test]
    fn non_empty_vault_is_not_seeded() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("existing.md"), "# hi").unwrap();
        ensure_vault_layout(dir.path()).unwrap();
        assert!(!dir.path().join("inbox/Welcome.md").exists());
    }
}
