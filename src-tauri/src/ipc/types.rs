//! Serde structs mirroring the TypeScript IPC contract
//! (packages/bridge-contract/src/ipc.ts). All structs serialize with
//! camelCase field names to match the shapes the React frontend expects.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    pub root: String,
    pub name: String,
    /// v2.15: marks an ephemeral "open folder temporarily" session (banner in
    /// the UI). Always absent until that feature is ported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalVaultEntry {
    pub root: String,
    pub name: String,
    pub last_opened_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMeta {
    /// Path relative to the vault root, always POSIX-style.
    pub path: String,
    /// File name without extension.
    pub title: String,
    /// One of inbox | quick | archive | trash.
    pub folder: String,
    pub sibling_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub size: u64,
    pub tags: Vec<String>,
    pub wikilinks: Vec<String>,
    /// Local files embedded via `![[file.png]]` / `![](file.png)` (v2.15).
    #[serde(default)]
    pub asset_embeds: Vec<String>,
    pub has_attachments: bool,
    pub excerpt: String,
    pub is_symlink: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteContent {
    #[serde(flatten)]
    pub meta: NoteMeta,
    /// Raw markdown body including any frontmatter.
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspaceProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub has_credential: bool,
    pub vault_path: Option<String>,
    pub last_connected_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspaceProfileInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub base_url: String,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub clear_auth_token: Option<bool>,
    #[serde(default)]
    pub vault_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetHotkeyResult {
    pub ok: bool,
    pub hotkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalFileContent {
    /// Absolute path on disk.
    pub path: String,
    /// File name including extension.
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveExternalFileResult {
    pub vault_root: String,
    pub rel_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultDemoTourResult {
    pub note_paths: Vec<String>,
    pub asset_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTemplateFile {
    pub source_path: String,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTemplateInput {
    pub slug: String,
    pub raw: String,
    #[serde(default)]
    pub previous_source_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteComment {
    pub id: String,
    pub note_path: String,
    pub anchor_start: i64,
    pub anchor_end: i64,
    pub anchor_text: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Resolved timestamp, or null when unresolved.
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultTask {
    pub id: String,
    pub source_path: String,
    pub note_title: String,
    pub note_folder: String,
    pub line_number: i64,
    pub task_index: i64,
    pub raw_text: String,
    pub content: String,
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    pub waiting: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetMeta {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub sibling_order: i64,
    pub size: u64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedAsset {
    pub path: String,
    pub name: String,
    pub undo_token: String,
    /// ISO timestamp of the deletion (v2.15; absent for pre-2.11 deletes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenExternalFileResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedAsset {
    pub name: String,
    pub path: String,
    pub markdown: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultTextSearchCapabilities {
    pub ripgrep: bool,
    pub fzf: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultTextSearchMatch {
    pub path: String,
    pub title: String,
    pub folder: String,
    pub line_number: i64,
    pub offset: i64,
    pub line_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodicNotesSettings {
    pub enabled: bool,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Passthrough for keys this backend doesn't interpret (titlePattern,
    /// locale, legacyPatterns, tasksDueOnNoteDate, …). The renderer's
    /// normalizer validates them; carrying them verbatim means a
    /// settings round-trip never drops newer-contract fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultSettings {
    pub primary_notes_location: String,
    pub daily_notes: PeriodicNotesSettings,
    pub weekly_notes: PeriodicNotesSettings,
    /// Map of `folder:subpath` → folder-icon id.
    pub folder_icons: std::collections::BTreeMap<String, String>,
    /// Passthrough for v2.15+ keys the backend doesn't interpret
    /// (monthlyNotes, folderColors, favorites, view, drawingsLocation,
    /// databasesLocation, tasksLocation, …). Kept verbatim so a
    /// get → set round-trip never drops them; the renderer validates.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultChangeEvent {
    /// "add" | "change" | "unlink".
    pub kind: String,
    /// Vault-relative POSIX path.
    pub path: String,
    /// inbox | quick | archive | trash.
    pub folder: String,
    /// "content" | "vault-settings" | "comments" (omitted for plain content).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    /// Top-level folder (inbox / quick / archive / trash).
    pub folder: String,
    /// POSIX subpath relative to the top-level folder, "" for the top itself.
    pub subpath: String,
    pub sibling_order: i64,
    pub is_symlink: bool,
}
