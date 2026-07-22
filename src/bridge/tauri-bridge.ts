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
import {
  DEFAULT_VAULT_SETTINGS,
  type AppUpdateState,
  type AssetMeta,
  type CliInstallStatus,
  type DeletedAsset,
  type DirectoryBrowseResult,
  type ExternalFileContent,
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
import type { AppConfigPortable } from '@zennotes/shared-domain/app-config'
import type { CustomTheme } from '@zennotes/shared-domain/custom-themes'
import type { DatabaseDoc, DatabaseSummary } from '@zennotes/shared-domain/databases'
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

const APP_INFO: ZenAppInfo = {
  name: 'zennotes-rs',
  productName: 'ZenNotes-rs',
  version: '2.1.0',
  description: 'ZenNotes-rs desktop',
  homepage: 'https://github.com/alf5/ZenNotes-rs',
  runtime: 'desktop'
}

function detectPlatform(): NodeJS.Platform {
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
  getAppInfo: (): ZenAppInfo => APP_INFO,
  platformSync: (): NodeJS.Platform => PLATFORM,

  // ---- Platform / system (M0 + M12) -------------------------------------
  platform: (): Promise<NodeJS.Platform> => invoke('app_platform'),
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
  generateDemoTour: (): Promise<VaultDemoTourResult> => invoke('vault_generate_demo_tour'),
  removeDemoTour: (): Promise<VaultDemoTourResult> => invoke('vault_remove_demo_tour'),
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
  scanTasks: (): Promise<VaultTask[]> => invoke('vault_scan_tasks'),
  scanTasksForPath: (relPath: string): Promise<VaultTask[]> =>
    invoke('vault_scan_tasks_for', { relPath }),
  writeNote: (relPath: string, body: string): Promise<NoteMeta> =>
    invoke('vault_write_note', { relPath, body }),
  appendToNote: (relPath: string, body: string, position: 'start' | 'end'): Promise<NoteMeta> =>
    invoke('vault_append_note', { relPath, body, position }),
  createNote: (folder: NoteFolder, title?: string, subpath?: string): Promise<NoteMeta> =>
    invoke('vault_create_note', { folder, title: title ?? null, subpath: subpath ?? null }),
  renameNote: (relPath: string, nextTitle: string): Promise<NoteMeta> =>
    invoke('vault_rename_note', { relPath, nextTitle }),
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
  renameAsset: (relPath: string, nextName: string): Promise<AssetMeta> =>
    invoke('vault_rename_asset', { relPath, nextName }),
  moveAsset: (relPath: string, targetDir: string): Promise<AssetMeta> =>
    invoke('vault_move_asset', { relPath, targetDir }),
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

  // ---- MCP / CLI / Raycast (M16, deferred) ------------------------------
  // The MCP server / `zen` CLI / Raycast extension need a bundled Node sidecar
  // (the MCP TS SDK + node-tikzjax) or a separate binary, which this build does
  // not ship yet. These return safe "unavailable" states so Settings renders
  // without crashing; no capability is flipped on.
  mcpGetRuntime: async (): Promise<McpServerRuntime> => ({
    command: '',
    args: [],
    env: {},
    entryPath: null
  }),
  mcpGetStatuses: async (): Promise<McpClientStatus[]> =>
    (['claude-code', 'claude-desktop', 'codex'] as McpClientId[]).map((id) => ({
      id,
      configPath: '',
      installed: false,
      upToDate: false,
      note: 'MCP integration is not available in this build yet.'
    })),
  mcpInstall: async (id: McpClientId): Promise<McpClientStatus> => ({
    id,
    configPath: '',
    installed: false,
    upToDate: false,
    note: 'MCP integration is not available in this build yet.'
  }),
  mcpUninstall: async (id: McpClientId): Promise<McpClientStatus> => ({
    id,
    configPath: '',
    installed: false,
    upToDate: false,
    note: 'MCP integration is not available in this build yet.'
  }),
  mcpGetInstructions: async (): Promise<McpInstructionsPayload> => ({
    defaultValue: '',
    current: '',
    isCustom: false,
    filePath: ''
  }),
  mcpSetInstructions: async (): Promise<McpInstructionsPayload> => ({
    defaultValue: '',
    current: '',
    isCustom: false,
    filePath: ''
  }),
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
  // Added by the v2.1.0 -> v2.15.0 re-vendor. Methods still awaiting their
  // Rust backend return the web bridge's degradation values, so the UI
  // renders and fails soft; the rest invoke their phase-A commands. See
  // GAP-ANALYSIS.md for the per-method plan. `convertObsidianExcalidraw`
  // (optional in the contract) is deliberately omitted: app-core
  // feature-detects it.

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

  // CSV databases: openDatabase -> null makes app-core forget the tab
  // gracefully; the mutators reject and every caller try/catches.
  openDatabase: async (): Promise<DatabaseDoc | null> => null,
  writeDatabaseRows: (): Promise<DatabaseDoc> => notImplemented('writeDatabaseRows'),
  writeDatabaseSchema: (): Promise<DatabaseDoc> => notImplemented('writeDatabaseSchema'),
  createDatabase: (): Promise<DatabaseDoc> => notImplemented('createDatabase'),
  renameDatabase: (): Promise<string> => notImplemented('renameDatabase'),
  createRecordPage: (): Promise<string> => notImplemented('createRecordPage'),
  listDatabases: async (): Promise<DatabaseSummary[]> => [],

  // Excalidraw drawings (.excalidraw files; editing goes through the
  // ordinary readNote/writeNote path).
  createExcalidraw: (folder: NoteFolder, subpath?: string, title?: string): Promise<NoteMeta> =>
    invoke('vault_create_excalidraw', {
      folder,
      subpath: subpath ?? null,
      title: title ?? null
    }),

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

  // Misc desktop surface. fetchLinkMetadata's {ok:false} stub is the exact
  // web-bridge degradation (bare bookmark card) until the Rust fetcher lands.
  revealFilePath: (absPath: string): Promise<void> =>
    invoke('vault_reveal_file_path', { absPath }),
  openExternalFile: (href: string): Promise<{ ok: boolean; error?: string }> =>
    invoke('vault_open_external_file', { href }),
  fetchLinkMetadata: async (url: string): Promise<LinkMetadata> => ({ url, ok: false }),
  openFolderTemporary: async (): Promise<void> => {},
  toggleDevTools: (): Promise<void> => invoke('devtools_toggle')
}

export function createTauriBridge(): ZenBridge {
  return installZenBridge(bridge)
}
