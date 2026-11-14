import type { ZenCapabilities } from '@zennotes/bridge-contract/bridge'

/**
 * Feature flags negotiated with the React frontend. The UI hides most
 * affordances whose capability is off; what it still shows is hidden or
 * reworded in tooling/vite/vendored-copy.ts.
 *
 *  - supportsLocalFilesystemPickers — M1 (vault open/pick)
 *  - supportsCustomTemplates        — M10 (templates CRUD)
 *  - supportsFloatingWindows        — M11 (multi-window)
 *  - supportsNativeMenus            — M12 (OS integration)
 *  - supportsRemoteWorkspace        — on: a vault on a ZenNotes server
 *    (src-tauri/src/remote + src/bridge/remote; docs/remote-workspace-plan.md)
 *  - supportsUpdater                — M15 (auto-updater)
 *
 * The v2.45 additions:
 *  - supportsWorkflows            — M18, `.zennotes/workflows` + apply journal
 *  - supportsCustomCodeLanguages  — M19, installed TextMate grammars
 *  - supportsCloudSync            — out of scope; see src/bridge/cloud-sync.ts
 *  - supportsHarper               — on: Harper's wasm is bundled by Vite and
 *    served same-origin from Tauri's asset protocol, so `bundledAssetUrl`
 *    passes the URL through untouched and no privileged scheme is needed
 *    (Electron needs `zen-harper://` only because its renderer is `file://`).
 *
 * The v2.73 additions:
 *  - supportsUndoFile             — on: undo history between launches, kept
 *    under the app's data folder by src-tauri/src/undo_history.rs (#793)
 *  - supportsAgentEditReview      — on: reviews the `.zennotes/agent-edits`
 *    records an assistant leaves through `zennotes-rs mcp`; see
 *    src/bridge/agent-edits.ts
 *  - supportsEncryptedBackupArchive — absent: cloud backups are out of scope
 *
 * The v2.85 additions:
 *  - supportsTemplateFolder       — on: vault.json's `templates.folder` survives
 *    a settings save (src-tauri/src/vault/settings.rs) and its notes stay out
 *    of Tasks and search (#1024)
 *  - supportsImport               — on: Obsidian, Logseq and Notion into the
 *    local vault (src/bridge/imports, src-tauri/src/imports)
 */
export const TAURI_CAPABILITIES: ZenCapabilities = {
  supportsUpdater: false,
  supportsNativeMenus: false,
  supportsFloatingWindows: true,
  supportsLocalFilesystemPickers: true,
  supportsRemoteWorkspace: true,
  supportsCliInstall: false,
  supportsCustomTemplates: true,
  supportsTemplateFolder: true,
  supportsCloudSync: false,
  supportsCustomCodeLanguages: true,
  supportsWorkflows: true,
  supportsHarper: true,
  supportsUndoFile: true,
  supportsAgentEditReview: true,
  supportsImport: true
}
