//! The vault as the MCP tools see it: upstream's `VaultBackend`
//! (apps/desktop/src/cli/backend.ts). Every tool runs against one, so a scope
//! (scoped.rs) applies to every handler without any handler checking it, and
//! a handler written against `Backend` gets the check for free.
//!
//! Only `LocalBackend` exists here: upstream's `RemoteBackend` (a vault on a
//! ZenNotes server, `--server` / `--token`) is not ported yet, so nothing
//! below has a remote branch.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::assets::{self, DeletedAsset, ImportedAsset};
use super::ops::{self, AssetMeta, FolderRef, NoteContent, NoteMeta, ReplaceResult, TextMatch};
use super::scoped::ScopedBackend;
use super::tasks::{self, VaultTask};
use crate::ipc::types::NoteComment;

pub trait Backend {
    /// The vault root on disk.
    fn root(&self) -> &Path;
    /// The scope decorator, when this is one (upstream's
    /// `backend instanceof ScopedBackend`).
    fn scoped(&self) -> Option<&ScopedBackend<'_>> {
        None
    }

    /// Whether the inbox is the vault root (`vaultLayout`).
    fn primary_at_root(&self) -> bool {
        ops::primary_notes_location(self.root()) == "root"
    }
    /// The vault's template folder (#1024), or None.
    fn template_folder(&self) -> Option<String> {
        ops::read_template_folder(self.root())
    }
    /// `.zennotes/vault.json` as stored.
    fn vault_settings(&self) -> Value {
        ops::vault_settings_raw(self.root())
    }

    fn list_notes(&self) -> Result<Vec<NoteMeta>, String>;
    fn list_folders(&self) -> Result<Vec<FolderRef>, String>;
    fn list_assets(&self) -> Result<Vec<AssetMeta>, String>;
    fn read_asset(&self, rel: &str) -> Result<Vec<u8>, String>;
    fn import_asset(&self, filename: &str, bytes: &[u8]) -> Result<ImportedAsset, String>;
    fn rename_asset(&self, rel: &str, name: &str) -> Result<AssetMeta, String>;
    fn move_asset(&self, rel: &str, target_dir: &str) -> Result<AssetMeta, String>;
    fn duplicate_asset(&self, rel: &str) -> Result<AssetMeta, String>;
    fn delete_asset(&self, rel: &str) -> Result<DeletedAsset, String>;
    fn list_deleted_assets(&self) -> Result<Vec<DeletedAsset>, String>;
    fn restore_deleted_asset(&self, deleted: &DeletedAsset) -> Result<AssetMeta, String>;
    /// The vault's own templates as (sourcePath, raw).
    fn list_templates(&self) -> Result<Vec<(String, String)>, String>;

    fn read_note(&self, rel: &str) -> Result<NoteContent, String>;
    fn write_note(&self, rel: &str, body: &str) -> Result<NoteMeta, String>;
    fn create_note(&self, folder: &str, title: Option<&str>, subpath: &str, body: Option<&str>) -> Result<NoteMeta, String>;
    fn append_to_note(&self, rel: &str, text: &str) -> Result<NoteMeta, String>;
    fn prepend_to_note(&self, rel: &str, text: &str) -> Result<NoteMeta, String>;
    fn insert_at_line(&self, rel: &str, line: f64, text: &str) -> Result<NoteMeta, String>;
    fn replace_in_note(&self, rel: &str, find: &str, replace: &str, all: bool) -> Result<ReplaceResult, String>;
    fn rename_note(&self, rel: &str, title: &str) -> Result<NoteMeta, String>;
    fn move_note(&self, rel: &str, folder: &str, subpath: &str) -> Result<NoteMeta, String>;
    fn duplicate_note(&self, rel: &str) -> Result<NoteMeta, String>;
    fn move_to_trash(&self, rel: &str) -> Result<NoteMeta, String>;
    fn restore_from_trash(&self, rel: &str) -> Result<NoteMeta, String>;
    fn archive_note(&self, rel: &str) -> Result<NoteMeta, String>;
    fn unarchive_note(&self, rel: &str) -> Result<NoteMeta, String>;
    fn delete_note(&self, rel: &str) -> Result<(), String>;
    fn empty_trash(&self) -> Result<(), String>;
    fn create_folder(&self, folder: &str, subpath: &str) -> Result<(), String>;
    fn rename_folder(&self, folder: &str, old: &str, new: &str) -> Result<String, String>;
    fn delete_folder(&self, folder: &str, subpath: &str) -> Result<(), String>;
    fn search_text(&self, query: &str, limit: f64) -> Result<Vec<TextMatch>, String>;
    fn backlinks(&self, rel: &str) -> Result<Vec<NoteMeta>, String>;
    fn scan_all_tasks(&self, include_excluded: bool) -> Result<Vec<VaultTask>, String>;
    fn scan_tasks_for_path(&self, rel: &str, include_excluded: bool) -> Result<Vec<VaultTask>, String>;
    fn toggle_task(&self, id: &str) -> Result<Option<VaultTask>, String>;
    fn list_comments(&self, rel: &str) -> Result<Vec<NoteComment>, String>;
    fn write_comments(&self, rel: &str, list: &[Value]) -> Result<Vec<NoteComment>, String>;
}

/// A vault on this machine: every method is ops bound to one root.
pub struct LocalBackend {
    pub root: PathBuf,
}

impl LocalBackend {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }
}

impl Backend for LocalBackend {
    fn root(&self) -> &Path {
        &self.root
    }
    fn list_notes(&self) -> Result<Vec<NoteMeta>, String> {
        Ok(ops::list_notes(&self.root))
    }
    fn list_folders(&self) -> Result<Vec<FolderRef>, String> {
        Ok(ops::list_folders(&self.root))
    }
    fn list_assets(&self) -> Result<Vec<AssetMeta>, String> {
        assets::list_assets(&self.root)
    }
    fn read_asset(&self, rel: &str) -> Result<Vec<u8>, String> {
        assets::read_asset(&self.root, rel)
    }
    fn import_asset(&self, filename: &str, bytes: &[u8]) -> Result<ImportedAsset, String> {
        assets::import_asset(&self.root, filename, bytes)
    }
    fn rename_asset(&self, rel: &str, name: &str) -> Result<AssetMeta, String> {
        assets::rename_asset(&self.root, rel, name, None)
    }
    fn move_asset(&self, rel: &str, target_dir: &str) -> Result<AssetMeta, String> {
        assets::move_asset(&self.root, rel, target_dir, None)
    }
    fn duplicate_asset(&self, rel: &str) -> Result<AssetMeta, String> {
        assets::duplicate_asset(&self.root, rel)
    }
    fn delete_asset(&self, rel: &str) -> Result<DeletedAsset, String> {
        assets::delete_asset(&self.root, rel)
    }
    fn list_deleted_assets(&self) -> Result<Vec<DeletedAsset>, String> {
        assets::list_deleted_assets(&self.root)
    }
    fn restore_deleted_asset(&self, deleted: &DeletedAsset) -> Result<AssetMeta, String> {
        assets::restore_deleted_asset(&self.root, &deleted.undo_token)
    }
    fn list_templates(&self) -> Result<Vec<(String, String)>, String> {
        assets::list_template_files(&self.root)
    }
    fn read_note(&self, rel: &str) -> Result<NoteContent, String> {
        ops::read_note(&self.root, rel)
    }
    fn write_note(&self, rel: &str, body: &str) -> Result<NoteMeta, String> {
        ops::write_note(&self.root, rel, body)
    }
    fn create_note(&self, folder: &str, title: Option<&str>, subpath: &str, body: Option<&str>) -> Result<NoteMeta, String> {
        ops::create_note(&self.root, folder, title, subpath, body)
    }
    fn append_to_note(&self, rel: &str, text: &str) -> Result<NoteMeta, String> {
        ops::append_to_note(&self.root, rel, text)
    }
    fn prepend_to_note(&self, rel: &str, text: &str) -> Result<NoteMeta, String> {
        ops::prepend_to_note(&self.root, rel, text)
    }
    fn insert_at_line(&self, rel: &str, line: f64, text: &str) -> Result<NoteMeta, String> {
        ops::insert_at_line(&self.root, rel, line, text)
    }
    fn replace_in_note(&self, rel: &str, find: &str, replace: &str, all: bool) -> Result<ReplaceResult, String> {
        ops::replace_in_note(&self.root, rel, find, replace, all)
    }
    fn rename_note(&self, rel: &str, title: &str) -> Result<NoteMeta, String> {
        ops::rename_note(&self.root, rel, title)
    }
    fn move_note(&self, rel: &str, folder: &str, subpath: &str) -> Result<NoteMeta, String> {
        ops::move_note(&self.root, rel, folder, subpath)
    }
    fn duplicate_note(&self, rel: &str) -> Result<NoteMeta, String> {
        ops::duplicate_note(&self.root, rel)
    }
    fn move_to_trash(&self, rel: &str) -> Result<NoteMeta, String> {
        ops::move_to_trash(&self.root, rel)
    }
    fn restore_from_trash(&self, rel: &str) -> Result<NoteMeta, String> {
        ops::restore_from_trash(&self.root, rel)
    }
    fn archive_note(&self, rel: &str) -> Result<NoteMeta, String> {
        ops::archive_note(&self.root, rel)
    }
    fn unarchive_note(&self, rel: &str) -> Result<NoteMeta, String> {
        ops::unarchive_note(&self.root, rel)
    }
    fn delete_note(&self, rel: &str) -> Result<(), String> {
        ops::delete_note(&self.root, rel)
    }
    fn empty_trash(&self) -> Result<(), String> {
        ops::empty_trash(&self.root);
        Ok(())
    }
    fn create_folder(&self, folder: &str, subpath: &str) -> Result<(), String> {
        ops::create_folder(&self.root, folder, subpath)
    }
    fn rename_folder(&self, folder: &str, old: &str, new: &str) -> Result<String, String> {
        ops::rename_folder(&self.root, folder, old, new)
    }
    fn delete_folder(&self, folder: &str, subpath: &str) -> Result<(), String> {
        ops::delete_folder(&self.root, folder, subpath)
    }
    fn search_text(&self, query: &str, limit: f64) -> Result<Vec<TextMatch>, String> {
        Ok(ops::search_text(&self.root, query, limit))
    }
    fn backlinks(&self, rel: &str) -> Result<Vec<NoteMeta>, String> {
        ops::backlinks(&self.root, rel)
    }
    fn scan_all_tasks(&self, include_excluded: bool) -> Result<Vec<VaultTask>, String> {
        Ok(tasks::scan_all_tasks(&self.root, include_excluded))
    }
    fn scan_tasks_for_path(&self, rel: &str, include_excluded: bool) -> Result<Vec<VaultTask>, String> {
        tasks::scan_tasks_for_path(&self.root, rel, include_excluded)
    }
    fn toggle_task(&self, id: &str) -> Result<Option<VaultTask>, String> {
        tasks::toggle_task(&self.root, id)
    }
    fn list_comments(&self, rel: &str) -> Result<Vec<NoteComment>, String> {
        Ok(ops::read_note_comments(&self.root, rel))
    }
    fn write_comments(&self, rel: &str, list: &[Value]) -> Result<Vec<NoteComment>, String> {
        ops::write_note_comments(&self.root, rel, list)
    }
}
