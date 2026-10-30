/**
 * Tauri implementation of the `window.zen` ZenBridge API.
 *
 * This is the third implementation of the bridge contract, alongside the
 * Electron preload (apps/desktop/src/preload/index.ts) and the web HTTP
 * bridge (apps/web/src/bridge/http-bridge.ts). The entire React frontend
 * runs unchanged on top of it.
 *
 * Methods backed by a Rust `#[tauri::command]` call `invoke(...)`. Methods
 * whose backend milestone has not landed yet return web-bridge-style safe
 * defaults / "unsupported" states, so the UI never crashes and never calls
 * into a command that does not exist. As each milestone lands, the matching
 * methods are switched from a default to an `invoke`, and the capability
 * flag in capabilities.ts is flipped on.
 *
 * Channel → command naming: the Electron IPC channel `vault:read-note`
 * becomes the Tauri command `vault_read_note`; push events
 * (`vault:on-change`) become Tauri events (`vault://change`).
 */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  installZenBridge,
  type ZenAppInfo,
  type ZenBridge,
  type ZenCapabilities
} from '@zennotes/bridge-contract/bridge'
import type { ZenPlatform } from '@zennotes/bridge-contract/platform'
import type { ExternalUrlResult } from '@zennotes/bridge-contract/application-links'
import {
  DEFAULT_VAULT_SETTINGS,
  type AgentEditRecord,
  type AgentEditSummary,
  type AppUpdateState,
  type AssetMeta,
  type CliInstallStatus,
  type DeletedAsset,
  type DirectoryBrowseResult,
  type ExternalFileContent,
  type ExternalFileLink,
  type FolderEntry,
  type ImportedAsset,
  type LinkMetadata,
  type LocalVaultEntry,
  type MoveExternalFileResult,
  type NoteComment,
  type NoteCommentInput,
  type NoteContent,
  type NoteFolder,
  type NoteMeta,
  type PastedImageInput,
  type RaycastExtensionStatus,
  type RemoteWorkspaceInfo,
  type RemoteWorkspaceProfile,
  type RemoteWorkspaceProfileInput,
  type ServerCapabilities,
  type ServerSessionStatus,
  type TikzRenderResponse,
  type TraySyncStatus,
  type VaultChangeEvent,
  type VaultDemoTourResult,
  type VaultInfo,
  type VaultSettings,
  type VaultTextSearchBackendPreference,
  type VaultTextSearchCapabilities,
  type VaultTextSearchMatch,
  type VaultTextSearchToolPaths
} from '@zennotes/bridge-contract/ipc'
import type { CustomTemplateFile, WriteTemplateInput } from '@zennotes/bridge-contract/templates'
import type {
  ApplyWorkflowInput,
  ExportWorkflowInput,
  ImportedWorkflowFile,
  WorkflowFile,
  WorkflowRunReceipt,
  WorkflowRunSummary,
  WorkflowUndoResult,
  WriteWorkflowInput
} from '@zennotes/bridge-contract/workflows'
import type { AppConfigPortable } from '@zennotes/shared-domain/app-config'
import type { CustomTheme } from '@zennotes/shared-domain/custom-themes'
import {
  extractObsidianExcalidrawScene,
  isObsidianExcalidrawMarkdown,
  isObsidianExcalidrawPath
} from '@zennotes/shared-domain/excalidraw'
import type {
  DatabaseDoc,
  DatabaseSeed,
  DatabaseSidecar,
  DatabaseSummary,
  DbRow
} from '@zennotes/shared-domain/databases'
import type {
  CustomCodeLanguage,
  CustomCodeLanguageInstallInput,
  CustomCodeLanguageUpdateInput
} from '@zennotes/shared-domain/custom-code-languages'
import type { Override } from '@zennotes/shared-domain/overrides'
import type { VaultTask } from '@zennotes/shared-domain/tasks'
import type {
  McpClientId,
  McpClientStatus,
  McpInstructionsPayload,
  McpServerRuntime
} from '@zennotes/shared-domain/mcp-clients'
import { TAURI_CAPABILITIES } from './capabilities'
import { resolveLocalAssetUrl, resolveVaultAssetUrl } from './asset-url'
import {
  ensureConfigFile,
  getConfigFilePathCached,
  getPortableConfigSnapshot,
  setPortableConfig,
  subscribeConfigChange
} from './portable-config'
import * as customCss from './custom-css'
import * as databases from './databases'
import { cloudSyncSurface } from './cloud-sync'
import * as codeLanguages from './code-languages'
import * as workflows from './workflows'
import * as tasks from './tasks'
import * as agentEdits from './agent-edits'
import { relocateAssetWithReferences, renameNoteWithLinks } from './link-rewrites'

const APP_INFO: ZenAppInfo = {
  name: 'zennotes-rs',
  productName: 'ZenNotes-rs',
  version: '2.73.0',
  description: 'ZenNotes-rs desktop',
  homepage: 'https://github.com/alf5/ZenNotes-rs',
  runtime: 'desktop',
  hostKind: 'desktop'
}

// What a bug report needs beside the version (#814): architecture, OS,
// renderer engine and install format, answered by Rust. getAppInfo() is
// synchronous and called many times at boot, so this is fetched once when the
// bridge is created and merged in when it arrives; `:version` and Settings >
// About read it long after. A field never answered stays out of the report.
let installInfo: Pick<ZenAppInfo, 'arch' | 'os' | 'engine' | 'install'> = {}

function detectPlatform(): ZenPlatform {
  const ua =
    (typeof navigator !== 'undefined' && (navigator.userAgent || navigator.platform)) || ''
  if (/Mac|iPhone|iPad|iPod/i.test(ua)) return 'darwin'
  if (/Win/i.test(ua)) return 'win32'
  return 'linux'
}
const PLATFORM = detectPlatform()

/** Subscribe to a Tauri event, returning a synchronous unsubscribe fn. */
function subscribe<T>(event: string, cb: (payload: T) => void): () => void {
  let unlisten = (): void => {}
  let cancelled = false
  void listen<T>(event, (e) => cb(e.payload)).then((fn) => {
    if (cancelled) fn()
    else unlisten = fn
  })
  return () => {
    cancelled = true
    unlisten()
  }
}

function notImplemented(name: string): Promise<never> {
  return Promise.reject(new Error(`ZenNotes-rs: "${name}" is not implemented yet`))
}

function unsupportedUpdateState(): AppUpdateState {
  return {
    phase: 'unsupported',
    // No in-app updater in this build; on Arch the AUR package owns the
    // install anyway, which is exactly the case `installable: false` names.
    installable: false,
    currentVersion: APP_INFO.version,
    availableVersion: null,
    releaseName: null,
    releaseDate: null,
    releaseNotes: null,
    progressPercent: null,
    transferredBytes: null,
    totalBytes: null,
    bytesPerSecond: null,
    message: 'Updates are not available in this build yet.'
  }
}

const SESSION_OK: ServerSessionStatus = {
  authenticated: true,
  authRequired: false,
  supportsSessionLogin: false
}

function unavailableCliStatus(): CliInstallStatus {
  return {
    available: false,
    reason: 'The ZenNotes-rs CLI is not bundled in this build yet.',
    defaultTarget: '',
    requiresSudo: false,
    targetOnPath: false,
    pathHint: null,
    installedAt: null,
    installedByThisApp: false,
    supportedPlatform: false
  }
}

function unavailableRaycastStatus(): RaycastExtensionStatus {
  return {
    available: false,
    reason: 'The Raycast extension is not bundled in this build yet.',
    supportedPlatform: PLATFORM === 'darwin',
    installed: false,
    upToDate: false,
    extensionPath: '',
    sourcePath: null,
    raycastInstalled: false,
    nodeAvailable: false,
    npmAvailable: false,
    nodePath: null,
    npmPath: null,
    nodeVersion: null,
    npmVersion: null,
    nodeMeetsMinimum: false,
    npmMeetsMinimum: false,
    installedVersion: null,
    bundledVersion: APP_INFO.version,
    lastInstalledAt: null
  }
}

const bridge: ZenBridge = {
  // ---- Sync, app-level ---------------------------------------------------
  getCapabilities: (): ZenCapabilities => TAURI_CAPABILITIES,
  getAppInfo: (): ZenAppInfo => ({ ...APP_INFO, ...installInfo }),
  platformSync: (): ZenPlatform => PLATFORM,

  // ---- Platform / system (M0 + M12) -------------------------------------
  platform: (): Promise<ZenPlatform> => invoke('app_platform'),
  listSystemFonts: (): Promise<string[]> => invoke('app_list_fonts'),
  getAppIconDataUrl: (): Promise<string | null> => invoke('app_icon_data_url'),
  zoomInApp: (): Promise<number> => invoke('app_zoom_in'),
  zoomOutApp: (): Promise<number> => invoke('app_zoom_out'),
  resetAppZoom: (): Promise<number> => invoke('app_zoom_reset'),
  renderTikz: async (): Promise<TikzRenderResponse> => ({
    ok: false,
    error: 'TikZ rendering is not available in this build yet.'
  }), // M16

  // ---- Updater (M15) -----------------------------------------------------
  getAppUpdateState: async (): Promise<AppUpdateState> => unsupportedUpdateState(),
  checkForAppUpdates: async (): Promise<AppUpdateState> => unsupportedUpdateState(),
  checkForAppUpdatesWithUi: async (): Promise<void> => {},
  downloadAppUpdate: async (): Promise<AppUpdateState> => unsupportedUpdateState(),
  installAppUpdate: async (): Promise<void> => {},

  // ---- Remote workspace (M14) -------------------------------------------
  getServerCapabilities: async (): Promise<ServerCapabilities | null> => null,
  getServerSession: async (): Promise<ServerSessionStatus> => SESSION_OK,
  loginServerSession: async (): Promise<ServerSessionStatus> => SESSION_OK,
  logoutServerSession: async (): Promise<ServerSessionStatus> => SESSION_OK,
  getRemoteWorkspaceInfo: async (): Promise<RemoteWorkspaceInfo | null> => null,
  connectRemoteWorkspace: (): Promise<{ vault: VaultInfo | null; capabilities: ServerCapabilities }> =>
    notImplemented('connectRemoteWorkspace'),
  disconnectRemoteWorkspace: async (): Promise<VaultInfo | null> => null,
  // Remote-workspace profile persistence is implemented (tokens in the OS
  // keychain); the live server connection is deferred (supportsRemoteWorkspace
  // stays off, so the UI keeps these hidden for now).
  listRemoteWorkspaceProfiles: (): Promise<RemoteWorkspaceProfile[]> =>
    invoke('workspace_list_remote_profiles'),
  saveRemoteWorkspaceProfile: (input: RemoteWorkspaceProfileInput): Promise<RemoteWorkspaceProfile> =>
    invoke('workspace_save_remote_profile', { input }),
  deleteRemoteWorkspaceProfile: (id: string): Promise<void> =>
    invoke('workspace_delete_remote_profile', { id }),
  connectRemoteWorkspaceProfile: (): Promise<{
    vault: VaultInfo | null
    capabilities: ServerCapabilities
  }> => notImplemented('connectRemoteWorkspaceProfile'),

  // ---- Vault selection (M1) ---------------------------------------------
  getCurrentVault: (): Promise<VaultInfo | null> => invoke('vault_get_current'),
  listLocalVaults: (): Promise<LocalVaultEntry[]> => invoke('vault_list_local'),
  openLocalVault: (root: string): Promise<VaultInfo | null> =>
    invoke('vault_open_local', { root }),
  closeVault: (): Promise<VaultInfo | null> => invoke('vault_close'),
  pickVault: (): Promise<VaultInfo | null> => invoke('vault_pick'),
  selectVaultPath: (targetPath: string): Promise<VaultInfo> =>
    invoke('vault_select_path', { targetPath }),
  browseServerDirectories: (_path?: string): Promise<DirectoryBrowseResult> =>
    notImplemented('browseServerDirectories'), // M14
  getVaultSettings: (): Promise<VaultSettings> => invoke('vault_get_settings'),
  setVaultSettings: (next: VaultSettings): Promise<VaultSettings> =>
    invoke('vault_set_settings', { next }),

  // ---- Notes / listing (M2 + M3) ----------------------------------------
  listNotes: (): Promise<NoteMeta[]> => invoke('vault_list_notes'),
  listFolders: (): Promise<FolderEntry[]> => invoke('vault_list_folders'),
  listAssets: (): Promise<AssetMeta[]> => invoke('vault_list_assets'),
  hasAssetsDir: (): Promise<boolean> => invoke('vault_has_assets_dir'),
  // Placed from vendored @shared/demo-tour-data; imported on demand.
  generateDemoTour: async (): Promise<VaultDemoTourResult> =>
    (await import('./demo-tour')).generateDemoTour(),
  removeDemoTour: async (): Promise<VaultDemoTourResult> =>
    (await import('./demo-tour')).removeDemoTour(),
  listTemplates: (): Promise<CustomTemplateFile[]> => invoke('vault_list_templates'),
  readTemplate: (sourcePath: string): Promise<string> =>
    invoke('vault_read_template', { sourcePath }),
  writeTemplate: (input: WriteTemplateInput): Promise<CustomTemplateFile> =>
    invoke('vault_write_template', { input }),
  deleteTemplate: (sourcePath: string): Promise<void> =>
    invoke('vault_delete_template', { sourcePath }),
  getVaultTextSearchCapabilities: (
    paths: VaultTextSearchToolPaths = {}
  ): Promise<VaultTextSearchCapabilities> =>
    invoke('vault_text_search_capabilities', { paths }),
  searchVaultText: (
    query: string,
    backend: VaultTextSearchBackendPreference = 'auto',
    paths: VaultTextSearchToolPaths = {}
  ): Promise<VaultTextSearchMatch[]> =>
    invoke('vault_search_text', { query, backend, paths }),
  readNote: (relPath: string): Promise<NoteContent> => invoke('vault_read_note', { relPath }), // M2
  readNoteComments: (relPath: string): Promise<NoteComment[]> =>
    invoke('vault_read_comments', { relPath }),
  writeNoteComments: (relPath: string, comments: NoteCommentInput[]): Promise<NoteComment[]> =>
    invoke('vault_write_comments', { relPath, comments }),
  // Parsed in the webview through vendored @shared/tasks; see tasks.ts.
  scanTasks: (): Promise<VaultTask[]> => tasks.scanTasks(),
  scanTasksForPath: (relPath: string): Promise<VaultTask[]> => tasks.scanTasksForPath(relPath),
  writeNote: (relPath: string, body: string): Promise<NoteMeta> =>
    invoke('vault_write_note', { relPath, body }),
  appendToNote: (relPath: string, body: string, position: 'start' | 'end'): Promise<NoteMeta> =>
    invoke('vault_append_note', { relPath, body, position }),
  createNote: (folder: NoteFolder, title?: string, subpath?: string): Promise<NoteMeta> =>
    invoke('vault_create_note', { folder, title: title ?? null, subpath: subpath ?? null }),
  // Inbound [[wikilinks]] follow the new name (upstream #111); see link-rewrites.ts.
  renameNote: (relPath: string, nextTitle: string): Promise<NoteMeta> =>
    renameNoteWithLinks(relPath, nextTitle),
  deleteNote: (relPath: string): Promise<void> => invoke('vault_delete_note', { relPath }),
  moveToTrash: (relPath: string): Promise<NoteMeta> => invoke('vault_move_to_trash', { relPath }),
  restoreFromTrash: (relPath: string): Promise<NoteMeta> =>
    invoke('vault_restore_from_trash', { relPath }),
  emptyTrash: (): Promise<void> => invoke('vault_empty_trash'),
  archiveNote: (relPath: string): Promise<NoteMeta> => invoke('vault_archive_note', { relPath }),
  unarchiveNote: (relPath: string): Promise<NoteMeta> =>
    invoke('vault_unarchive_note', { relPath }),
  duplicateNote: (relPath: string): Promise<NoteMeta> =>
    invoke('vault_duplicate_note', { relPath }),
  exportNotePdf: async (): Promise<string | null> => null, // M16
  revealNote: (relPath: string): Promise<void> => invoke('vault_reveal_note', { relPath }),
  revealNoteTarget: (relPath: string): Promise<void> =>
    invoke('vault_reveal_note_target', { relPath }),
  moveNote: (relPath: string, targetFolder: NoteFolder, targetSubpath: string): Promise<NoteMeta> =>
    invoke('vault_move_note', { relPath, targetFolder, targetSubpath }),
  importFilesToNote: (notePath: string, sourcePaths: string[]): Promise<ImportedAsset[]> =>
    invoke('vault_import_files', { notePath, sourcePaths }),
  importPastedImage: (input: PastedImageInput): Promise<ImportedAsset> => {
    const bytes =
      input.data instanceof ArrayBuffer ? new Uint8Array(input.data) : new Uint8Array(input.data)
    return invoke('vault_import_pasted_image', {
      data: Array.from(bytes),
      mimeType: input.mimeType,
      suggestedName: input.suggestedName ?? null
    })
  },

  // ---- Assets (M7) ------------------------------------------------------
  // References to the asset follow it (#785); see link-rewrites.ts.
  renameAsset: (relPath: string, nextName: string): Promise<AssetMeta> =>
    relocateAssetWithReferences(relPath, () => invoke('vault_rename_asset', { relPath, nextName })),
  moveAsset: (relPath: string, targetDir: string): Promise<AssetMeta> =>
    relocateAssetWithReferences(relPath, () => invoke('vault_move_asset', { relPath, targetDir })),
  duplicateAsset: (relPath: string): Promise<AssetMeta> =>
    invoke('vault_duplicate_asset', { relPath }),
  deleteAsset: (relPath: string): Promise<DeletedAsset> =>
    invoke('vault_delete_asset', { relPath }),
  restoreDeletedAsset: (asset: DeletedAsset): Promise<AssetMeta> =>
    invoke('vault_restore_deleted_asset', { asset }),

  // ---- Folders (M5) -----------------------------------------------------
  createFolder: (folder: NoteFolder, subpath: string): Promise<void> =>
    invoke('vault_create_folder', { folder, subpath }),
  renameFolder: (folder: NoteFolder, oldSubpath: string, newSubpath: string): Promise<string> =>
    invoke('vault_rename_folder', { folder, oldSubpath, newSubpath }),
  deleteFolder: (folder: NoteFolder, subpath: string): Promise<void> =>
    invoke('vault_delete_folder', { folder, subpath }),
  duplicateFolder: (folder: NoteFolder, subpath: string): Promise<string> =>
    invoke('vault_duplicate_folder', { folder, subpath }),
  revealFolder: (folder: NoteFolder, subpath: string): Promise<void> =>
    invoke('vault_reveal_folder', { folder, subpath }),
  revealFolderTarget: (folder: NoteFolder, subpath: string): Promise<void> =>
    invoke('vault_reveal_folder_target', { folder, subpath }),
  revealAssetsDir: (): Promise<void> => invoke('vault_reveal_assets_dir'),

  // ---- Sync asset URL + drag-drop ---------------------------------------
  getPathForFile: (): string | null => null, // M11 (drag-drop event + token map)
  resolveLocalAssetUrl,
  resolveVaultAssetUrl,

  // ---- Push events -------------------------------------------------------
  onVaultChange: (cb: (ev: VaultChangeEvent) => void): (() => void) =>
    subscribe<VaultChangeEvent>('vault://change', cb),
  onOpenSettings: (cb: () => void): (() => void) =>
    subscribe<unknown>('app://open-settings', () => cb()),
  onOpenNoteRequested: (cb: (relPath: string) => void): (() => void) =>
    subscribe<string>('app://open-note', cb),
  onAppUpdateState: (cb: (state: AppUpdateState) => void): (() => void) =>
    subscribe<AppUpdateState>('app://update-state', cb),
  notifyRendererReady: (): void => {
    void invoke('app_renderer_ready').catch(() => {})
  },

  // ---- Window control (basic now, full multi-window at M11) -------------
  windowMinimize: (): void => {
    void getCurrentWindow().minimize().catch(() => {})
  },
  windowToggleMaximize: (): void => {
    void getCurrentWindow().toggleMaximize().catch(() => {})
  },
  windowClose: (): void => {
    void getCurrentWindow().close().catch(() => {})
  },
  openNoteWindow: (relPath: string): Promise<void> => invoke('window_open_note', { relPath }),
  openVaultWindow: (root?: string): Promise<VaultInfo | null> =>
    invoke('window_open_vault', { root: root ?? null }),
  readExternalFile: (): Promise<ExternalFileContent> => invoke('app_read_external_file'),
  writeExternalFile: (body: string): Promise<void> => invoke('app_write_external_file', { body }),
  moveExternalFileToVault: (): Promise<MoveExternalFileResult> =>
    invoke('app_move_external_file_to_vault'),
  openMarkdownFile: (absPath: string): Promise<boolean> =>
    invoke('app_open_markdown_file', { absPath }),
  toggleQuickCapture: (): Promise<void> => invoke('window_toggle_quick_capture'),
  getQuickCaptureHotkey: (): Promise<string> => invoke('app_get_quick_capture_hotkey'),
  setQuickCaptureHotkey: (
    hotkey: string
  ): Promise<{ ok: boolean; hotkey: string; error?: string }> =>
    invoke('app_set_quick_capture_hotkey', { hotkey }),
  getQuickCapturePinned: (): Promise<boolean> => invoke('app_get_quick_capture_pinned'),
  setQuickCapturePinned: (pinned: boolean): Promise<boolean> =>
    invoke('app_set_quick_capture_pinned', { pinned }),

  // ---- MCP -----------------------------------------------------------------
  // The server is this binary run as `zennotes-rs mcp` (src-tauri/src/mcp);
  // these install it into a client's config and edit the instructions it
  // sends. The server-token members stay absent: there are no remote vaults.
  mcpGetRuntime: (): Promise<McpServerRuntime> => invoke('mcp_get_runtime'),
  mcpGetStatuses: (): Promise<McpClientStatus[]> => invoke('mcp_get_statuses'),
  mcpInstall: (id: McpClientId): Promise<McpClientStatus> => invoke('mcp_install', { id }),
  mcpUninstall: (id: McpClientId): Promise<McpClientStatus> => invoke('mcp_uninstall', { id }),
  mcpGetInstructions: (): Promise<McpInstructionsPayload> => invoke('mcp_get_instructions'),
  mcpSetInstructions: (next: string | null): Promise<McpInstructionsPayload> =>
    invoke('mcp_set_instructions', { next }),

  // ---- CLI / Raycast (deferred) -------------------------------------------
  // The `zen` CLI and the Raycast extension are not part of this build; these
  // return safe "unavailable" states so Settings renders.
  cliGetStatus: async (): Promise<CliInstallStatus> => unavailableCliStatus(),
  cliInstall: async (): Promise<CliInstallStatus> => unavailableCliStatus(),
  cliUninstall: async (): Promise<CliInstallStatus> => unavailableCliStatus(),
  raycastGetStatus: async (): Promise<RaycastExtensionStatus> => unavailableRaycastStatus(),
  raycastInstall: async (): Promise<RaycastExtensionStatus> => unavailableRaycastStatus(),

  // ---- Clipboard (basic now, plugin at M12) -----------------------------
  clipboardWriteText: (text: string): void => {
    try {
      void navigator.clipboard?.writeText(text)
    } catch {
      /* ignore */
    }
  },
  clipboardReadText: (): string => '', // M12 (sync read is not available in the webview)

  // ---- v2.15 contract surface (M17) --------------------------------------
  // Added by the v2.1.0 -> v2.15.0 re-vendor; the whole surface is
  // implemented (see GAP-ANALYSIS.md for the per-method history). The one
  // intentional stub left is `listDatabases` — dead code upstream.

  // Workspace state (<vault>/.zennotes/workspace.json, raw JSON strings —
  // the renderer owns the schema and reconciles newest-wins vs localStorage).
  readWorkspaceState: (): Promise<string | null> => invoke('workspace_state_read'),
  writeWorkspaceState: (json: string): Promise<void> => invoke('workspace_state_write', { json }),
  rootContentHiddenByInboxMode: (): Promise<boolean> => invoke('vault_root_content_hidden'),

  // Portable config (~/.config/zennotes/config.toml). The TOML format lives
  // in portable-config.ts (upstream's serializer, verbatim); Rust is the
  // file layer + watcher. initPortableConfig() runs in main.tsx before React
  // mounts, so the synchronous snapshot has real data at first paint — {}
  // when the file is absent, which triggers app-core to seed it from
  // localStorage (the v2.1.0 → config-file migration, for free).
  getConfigSync: (): AppConfigPortable | null => getPortableConfigSnapshot(),
  setConfig: (next: AppConfigPortable): Promise<void> => setPortableConfig(next),
  getConfigPath: async (): Promise<string | null> => getConfigFilePathCached(),
  revealConfigFile: async (): Promise<void> => {
    const path = await ensureConfigFile()
    if (path) await invoke('vault_reveal_file_path', { absPath: path })
  },
  onConfigChange: (cb: (next: AppConfigPortable) => void): (() => void) =>
    subscribeConfigChange(cb),

  // CSV databases. The parse/infer/serialize logic runs in databases.ts via
  // the vendored shared-domain functions; Rust does the vault file IO.
  // listDatabases stays a stub: nothing in app-core ever calls it (upstream
  // only wires it for the web bridge).
  openDatabase: (relPath: string): Promise<DatabaseDoc | null> => databases.openDatabase(relPath),
  writeDatabaseRows: (relPath: string, rows: DbRow[]): Promise<DatabaseDoc> =>
    databases.writeDatabaseRows(relPath, rows),
  writeDatabaseSchema: (
    relPath: string,
    sidecar: DatabaseSidecar,
    rows: DbRow[]
  ): Promise<DatabaseDoc> => databases.writeDatabaseSchema(relPath, sidecar, rows),
  createDatabase: (
    folder: NoteFolder,
    subpath: string,
    title?: string,
    seed?: DatabaseSeed
  ): Promise<DatabaseDoc> => databases.createDatabase(folder, subpath, title, seed),
  renameDatabase: (csvPath: string, newTitle: string): Promise<string> =>
    databases.renameDatabase(csvPath, newTitle),
  createRecordPage: (csvPath: string, title: string, body: string): Promise<string> =>
    databases.createRecordPage(csvPath, title, body),
  listDatabases: async (): Promise<DatabaseSummary[]> => [],

  // Excalidraw drawings (.excalidraw files; editing goes through the
  // ordinary readNote/writeNote path).
  createExcalidraw: (folder: NoteFolder, subpath?: string, title?: string): Promise<NoteMeta> =>
    invoke('vault_create_excalidraw', {
      folder,
      subpath: subpath ?? null,
      title: title ?? null
    }),
  // Obsidian `.excalidraw.md` conversion (upstream vault.ts:3184). The scene
  // extraction (incl. lz-string decompression) is vendored pure TS; Rust
  // writes the deduped sibling `.excalidraw` and returns its meta. The
  // original markdown is left intact.
  convertObsidianExcalidraw: async (relPath: string): Promise<NoteMeta> => {
    const markdown = await invoke<string | null>('db_read_text', { relPath })
    if (markdown === null) throw new Error(`Drawing not found: ${relPath}`)
    if (!isObsidianExcalidrawPath(relPath) && !isObsidianExcalidrawMarkdown(markdown)) {
      throw new Error('This file is not an Obsidian Excalidraw drawing.')
    }
    const scene = extractObsidianExcalidrawScene(markdown)
    if (!scene) throw new Error('Could not read an Excalidraw scene from this file.')
    const fileName = relPath.split('/').pop() ?? relPath
    const base =
      (fileName.toLowerCase().endsWith('.excalidraw.md')
        ? fileName.slice(0, -'.excalidraw.md'.length)
        : fileName.replace(/\.[^.]*$/, '')) || 'Untitled drawing'
    const dirRel = relPath.includes('/') ? relPath.slice(0, relPath.lastIndexOf('/')) : ''
    return invoke('vault_write_drawing', {
      dirRel,
      baseTitle: base,
      body: JSON.stringify(scene, null, 2)
    })
  },

  // Deleted-assets store (.zennotes/deleted-assets/<uuid>/ + sidecar).
  listDeletedAssets: (): Promise<DeletedAsset[]> => invoke('vault_list_deleted_assets'),
  purgeDeletedAsset: (undoToken: string): Promise<void> =>
    invoke('vault_purge_deleted_asset', { undoToken }),
  emptyDeletedAssets: (): Promise<void> => invoke('vault_empty_deleted_assets'),

  // Custom themes + CSS overrides (~/.config/zennotes/{themes,overrides}).
  // Rust scans/watches; parsing + scaffolding run in custom-css.ts through
  // the vendored shared-domain functions.
  listCustomThemes: (): Promise<CustomTheme[]> => customCss.listCustomThemes(),
  getCustomThemesDir: (): Promise<string | null> => customCss.getCustomThemesDir(),
  revealCustomThemesDir: (slug?: string): Promise<void> => customCss.revealCustomThemesDir(slug),
  deleteCustomTheme: (slug: string): Promise<void> => customCss.deleteCustomTheme(slug),
  createCustomTheme: (input: { name?: string }): Promise<string | null> =>
    customCss.createCustomTheme(input),
  onCustomThemesChange: (cb: (next: CustomTheme[]) => void): (() => void) =>
    customCss.subscribeCustomThemesChange(cb),
  listOverrides: (): Promise<Override[]> => customCss.listOverrides(),
  revealOverridesDir: (name?: string): Promise<void> => customCss.revealOverridesDir(name),
  deleteOverride: (name: string): Promise<void> => customCss.deleteOverride(name),
  onOverridesChange: (cb: (next: Override[]) => void): (() => void) =>
    customCss.subscribeOverridesChange(cb),

  // Misc desktop surface.
  revealFilePath: (absPath: string): Promise<void> =>
    invoke('vault_reveal_file_path', { absPath }),
  openExternalFile: (href: string): Promise<{ ok: boolean; error?: string }> =>
    invoke('vault_open_external_file', { href }),
  fetchLinkMetadata: (url: string): Promise<LinkMetadata> =>
    invoke('vault_fetch_link_metadata', { url }),
  openFolderTemporary: (absPath: string): Promise<void> =>
    invoke('app_open_folder_temporary', { rawPath: absPath }),
  toggleDevTools: (): Promise<void> => invoke('devtools_toggle'),

  // ---- v2.45 contract surface (M18+) -------------------------------------
  // Added by the v2.15.0 -> v2.45.0 re-vendor. See GAP-ANALYSIS.md for the
  // per-cluster plan; the stubs below reproduce the web bridge's degradation
  // so an unimplemented feature is hidden by its capability flag rather than
  // crashing, and a stray call fails with a message instead of hitting a
  // Tauri command that was never registered.

  // Cloud sync (33 members) — out of scope for this port; see cloud-sync.ts.
  ...cloudSyncSurface,

  // Boot retry. Upstream re-attempts a remote workspace that was unreachable
  // at launch; this build has no remote workspace (supportsRemoteWorkspace is
  // false), so the only workspace there is to retry is the local vault and
  // reporting the current one is both truthful and idempotent.
  retryWorkspaceBoot: (): Promise<VaultInfo | null> => invoke('vault_get_current'),

  // Workflows — M18.
  listWorkflows: (): Promise<WorkflowFile[]> => invoke('vault_list_workflows'),
  writeWorkflow: (input: WriteWorkflowInput): Promise<WorkflowFile> =>
    invoke('vault_write_workflow', {
      slug: input.slug,
      raw: input.raw,
      previousSourcePath: input.previousSourcePath ?? null
    }),
  deleteWorkflow: (sourcePath: string): Promise<void> =>
    invoke('vault_delete_workflow', { sourcePath }),
  exportWorkflow: (input: ExportWorkflowInput): Promise<string | null> =>
    invoke('vault_export_workflow', { suggestedName: input.suggestedName, raw: input.raw }),
  importWorkflowFile: (): Promise<ImportedWorkflowFile | null> =>
    invoke('vault_import_workflow_file'),
  applyWorkflow: (input: ApplyWorkflowInput): Promise<WorkflowRunReceipt> =>
    workflows.applyWorkflow(input),
  undoWorkflowRun: (runId: string): Promise<WorkflowUndoResult> => workflows.undoWorkflowRun(runId),
  listWorkflowRuns: (): Promise<WorkflowRunSummary[]> => workflows.listWorkflowRuns(),
  deleteWorkflowRuns: (workflowId: string): Promise<number> =>
    workflows.deleteWorkflowRuns(workflowId),

  // Custom code languages (installed TextMate grammars) — M19.
  listCustomCodeLanguages: (): Promise<CustomCodeLanguage[]> => codeLanguages.list(),
  installCustomCodeLanguage: (
    input: CustomCodeLanguageInstallInput
  ): Promise<CustomCodeLanguage> => codeLanguages.install(input),
  updateCustomCodeLanguage: (input: CustomCodeLanguageUpdateInput): Promise<CustomCodeLanguage> =>
    codeLanguages.update(input),
  revealCustomCodeLanguagesDir: (id?: string): Promise<void> => codeLanguages.revealDir(id),
  deleteCustomCodeLanguage: (id: string): Promise<void> => codeLanguages.remove(id),
  onCustomCodeLanguagesChange: (cb: (next: CustomCodeLanguage[]) => void): (() => void) =>
    codeLanguages.subscribe(cb),

  // Word export. Upstream renders in the main process with the `docx` npm
  // package (apps/desktop/src/main/note-docx.ts); no Rust equivalent yet, so
  // this is the web bridge's honest message rather than a silent no-op.
  exportNoteDocx: (): Promise<string | null> =>
    Promise.reject(new Error('Word export is not implemented in this build yet.')),

  // Open a VAULT asset with the OS default app. The vault is always local
  // here, so this is a path resolve away from the existing opener.
  openAssetExternally: (relPath: string): Promise<{ ok: boolean; error?: string }> =>
    invoke('vault_open_asset_externally', { relPath }),

  // Read a vault asset without handing the renderer a host path.
  readVaultAssetBase64: (assetPath: string): Promise<string> =>
    invoke('vault_read_asset_base64', { assetPath }),

  // Escape pressed inside a cross-origin subframe. Electron relays it from
  // the main process via a WebContents key handler; WRY exposes no equivalent
  // hook below the page, so this build has no way to observe it. Optional in
  // the contract precisely so a host without the hook can omit it — omitted
  // rather than registered as a listener that could never fire.
  // onFrameEscape: intentionally absent.

  // Follow a link out of a standalone external-file window.
  followExternalFileLink: (link: ExternalFileLink): Promise<{ ok: boolean; error?: string }> =>
    invoke('external_file_follow_link', { link }),

  // Native "Open File…" for a markdown file, opened the vault-aware way.
  openFileDialog: (): Promise<boolean> => invoke('app_open_file_dialog'),

  // ---- v2.73 contract surface --------------------------------------------
  // Added by the v2.45.0 -> v2.73.0 re-vendor; see GAP-ANALYSIS.md.

  // Open a URL from a note. Rust owns the permission: http(s)/mailto/tel, or
  // an application scheme enabled in Settings (external_application_schemes).
  openExternalUrl: (url: string): Promise<ExternalUrlResult> =>
    invoke('app_open_external_url', { url }),

  // Undo history kept between launches (#793): machine-local, under the app's
  // data folder, never in the vault; Rust hashes the note path into a name.
  readNoteUndoHistory: (path: string): Promise<string | null> =>
    invoke('undo_history_read', { notePath: path }),
  writeNoteUndoHistory: (path: string, json: string | null): Promise<void> =>
    invoke('undo_history_write', { notePath: path, json }),
  clearNoteUndoHistories: (): Promise<void> => invoke('undo_history_clear'),

  // Agent edit records left by an assistant editing through upstream's MCP
  // server, reviewed change by change; see agent-edits.ts.
  listAgentEdits: (): Promise<AgentEditSummary[]> => agentEdits.listAgentEdits(),
  readAgentEdit: (relPath: string): Promise<AgentEditRecord | null> =>
    agentEdits.readAgentEdit(relPath),
  writeAgentEdit: (relPath: string, record: AgentEditRecord | null): Promise<void> =>
    agentEdits.writeAgentEdit(relPath, record),

  // Close to the tray (#947): closing the last workspace window hides it and
  // the tray icon brings it back, so the Quick capture hotkey keeps working.
  // Rust owns the setting (machine-local) and refuses to hide while it is
  // off; without an appindicator library on Linux it stays off.
  getKeepRunningInTray: (): Promise<boolean> => invoke('app_get_keep_running_in_tray'),
  setKeepRunningInTray: (enabled: boolean): Promise<boolean> =>
    invoke('app_set_keep_running_in_tray', { enabled }),
  hideWindowToTray: (): Promise<boolean> => invoke('window_hide_to_tray'),
  reportTraySyncStatus: (status: TraySyncStatus | null): void => {
    void invoke('app_tray_sync_status', { status }).catch(() => {})
  },
  quitApp: (): void => {
    void invoke('app_quit').catch(() => {})
  }

  // Deliberately absent (all optional in the contract, so app-core hides
  // what they drive): sendUsageCount (upstream's anonymous install count goes
  // to zennotes.org and would count this port as upstream), the MCP server
  // token members (no remote vaults to authenticate to) and the managed-CLI
  // update members (no CLI ships here),
  // and the cloud encryption members (cloud sync is out of scope).
}

/**
 * Send `window.open` of an outside URL to the OS, the way Electron's
 * `setWindowOpenHandler` does upstream (`openAllowedExternalUrl`). app-core
 * opens every external link this way (preview links, Help, release notes),
 * and WebKitGTK under Tauri does nothing with such a request: no window, no
 * browser. The URL goes through `app_open_external_url`, so the same Rust gate
 * decides (http(s)/mailto/tel, or an application scheme enabled in Settings).
 * Anything else, a relative URL or the app's own origin, keeps the native
 * behaviour.
 */
function routeExternalWindowOpens(): void {
  const nativeOpen = window.open.bind(window)
  window.open = (url?: string | URL, target?: string, features?: string): WindowProxy | null => {
    const href = url === undefined ? '' : String(url).trim()
    let external = false
    try {
      const parsed = new URL(href)
      external = parsed.origin !== window.location.origin && parsed.protocol !== 'tauri:'
    } catch {
      external = false
    }
    if (!external) return nativeOpen(url, target, features)
    void invoke('app_open_external_url', { url: href }).catch(() => {})
    return null
  }
}

export function createTauriBridge(): ZenBridge {
  routeExternalWindowOpens()
  void invoke<typeof installInfo>('app_install_info')
    .then((info) => {
      installInfo = info
    })
    .catch(() => {})
  return installZenBridge(bridge)
}
