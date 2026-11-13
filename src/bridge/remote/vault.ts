/**
 * The remote branch of every vault bridge member: what upstream's main process
 * does in each IPC handler under `if (isRemoteWorkspaceActive())`
 * (apps/desktop/src/main/index.ts, VAULT_* handlers). `tauri-bridge.ts` routes
 * here while a session is on.
 *
 * Kept as upstream has it, refusals and capability checks included, so a
 * server that lacks a feature gets the same message here as in upstream's app.
 * Asset rename and move have no capability check: they predate the 2.24
 * asset family that `supportsAssetOps` announces.
 */

import type {
  AgentEditRecord,
  AgentEditSummary,
  AssetMeta,
  DeletedAsset,
  DirectoryBrowseResult,
  FolderEntry,
  ImportedAsset,
  NoteComment,
  NoteCommentInput,
  NoteContent,
  NoteFolder,
  NoteMeta,
  PastedImageInput,
  ServerCapabilities,
  VaultDemoTourResult,
  VaultSettings,
  VaultTextSearchBackendPreference,
  VaultTextSearchCapabilities,
  VaultTextSearchMatch,
  VaultTextSearchToolPaths
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
import { createDatabaseOps, type DatabaseOps } from '@zennotes/shared-domain/database-ops'
import type {
  DatabaseDoc,
  DatabaseSeed,
  DatabaseSidecar,
  DatabaseSummary,
  DbRow
} from '@zennotes/shared-domain/databases'
import { pastedImageFilename } from '@zennotes/shared-domain/pasted-image'
import { createAbsenceAwareReader } from '@zennotes/shared-domain/remote-absence'
import type { VaultTask } from '@zennotes/shared-domain/tasks'
import { invoke } from '../ipc'
import { RemoteServerClient } from './client'
import { RemoteRequestError } from './errors'
import { remoteWorkspaceInfo } from './state'

export const REVEAL_LOCAL_ONLY = 'Reveal in file manager is only available for local vaults.'
export const WORKFLOW_FILES_LOCAL_ONLY = 'Workflows are unavailable on remote vaults'
export const OBSIDIAN_DRAWINGS_LOCAL_ONLY =
  'Converting Obsidian drawings is only available for local vaults.'

const client = new RemoteServerClient({
  // `sendRemoteResync`: every window re-pulls the list that fell behind.
  onStalePath: () => {
    void invoke('remote_resync').catch(() => {})
  }
})

/** The capabilities the session connected with (refreshed on vault select). */
function capabilities(): ServerCapabilities | null {
  return remoteWorkspaceInfo()?.capabilities ?? null
}

async function requireRemoteWorkflows(): Promise<RemoteServerClient> {
  if (!(await client.supportsWorkflows())) {
    throw new Error(
      'This ZenNotes server does not support workflows yet. Update the server and reconnect.'
    )
  }
  return client
}

function requireRemoteTemplates(action: string): RemoteServerClient {
  if (!capabilities()?.supportsCustomTemplates) {
    throw new Error(
      `${action} needs ZenNotes server 2.46 or later. Update the server and reconnect this workspace.`
    )
  }
  return client
}

/** The delete/duplicate/restore endpoints shipped with server 2.24; against
 *  an older server the request would just 404. Name the real remedy instead. */
function requireRemoteAssetOps(action: string): RemoteServerClient {
  if (!capabilities()?.supportsAssetOps) {
    throw new Error(
      `${action} needs a newer ZenNotes server. Update the server and reconnect this workspace.`
    )
  }
  return client
}

// Databases compose from the note and folder endpoints, as on the web and in
// upstream's desktop (#499), so any server version works. Rebuilt for each
// connection, because the absence reader learns about the server it asks.
let databaseOps: { key: string; ops: DatabaseOps } | null = null

function databaseOpsForRemote(): DatabaseOps {
  const info = remoteWorkspaceInfo()
  const key = `${info?.baseUrl ?? ''}\n${info?.profileId ?? ''}`
  if (databaseOps?.key === key) return databaseOps.ops
  const ops = createDatabaseOps({
    // 404 is absence; anything else is absence only if THIS server cannot
    // say 404 for a missing file, which the reader settles by asking it once.
    readFileTextOrNull: createAbsenceAwareReader({
      read: async (relPath) => (await client.readNote(relPath)).body,
      statusOf: (err) => (err instanceof RemoteRequestError ? err.status : null),
      serverReportsMissingAsNotFound: () => capabilities()?.reportsMissingAsNotFound === true
    }),
    writeFile: async (relPath, text) => {
      await client.writeNote(relPath, text)
    },
    createFolder: (folder, subpath) => client.createFolder(folder, subpath),
    renameFolder: (folder, oldSubpath, newSubpath) =>
      client.renameFolder(folder, oldSubpath, newSubpath),
    listFolders: () => client.listFolders(),
    // Errors propagate: answering "defaults" for an unreachable server sends
    // every database path composition to a literal `inbox/`.
    vaultLayout: async () => {
      const settings = await client.getVaultSettings()
      return {
        primaryNotesAtRoot: settings.primaryNotesLocation === 'root',
        systemFolderPaths: settings.systemFolderPaths
      }
    }
  })
  databaseOps = { key, ops }
  return ops
}

export const remoteVault = {
  browseServerDirectories: (path = ''): Promise<DirectoryBrowseResult> =>
    invoke('workspace_browse_server_directories', { path }),

  getVaultSettings: (): Promise<VaultSettings> => client.getVaultSettings(),
  setVaultSettings: (next: VaultSettings): Promise<VaultSettings> => client.setVaultSettings(next),

  // Remote workspaces keep their session state server-side.
  readWorkspaceState: async (): Promise<string | null> => null,
  writeWorkspaceState: async (_json: string): Promise<void> => {},
  rootContentHiddenByInboxMode: async (): Promise<boolean> => false,

  listNotes: (): Promise<NoteMeta[]> => client.listNotes(),
  listFolders: (): Promise<FolderEntry[]> => client.listFolders(),
  listAssets: (): Promise<AssetMeta[]> => client.listAssets(),
  hasAssetsDir: (): Promise<boolean> => client.hasAssetsDir(),
  readVaultAssetBase64: (assetPath: string): Promise<string> => {
    const rel = String(assetPath ?? '').trim()
    if (!rel) return Promise.reject(new Error('Asset path is required.'))
    return client.readAssetBase64(rel)
  },
  generateDemoTour: (): Promise<VaultDemoTourResult> => client.generateDemoTour(),
  removeDemoTour: (): Promise<VaultDemoTourResult> => client.removeDemoTour(),

  listWorkflows: async (): Promise<WorkflowFile[]> =>
    (await client.supportsWorkflows()) ? await client.listWorkflows() : [],
  writeWorkflow: async (input: WriteWorkflowInput): Promise<WorkflowFile> =>
    (await requireRemoteWorkflows()).writeWorkflow(input),
  deleteWorkflow: async (sourcePath: string): Promise<void> =>
    (await requireRemoteWorkflows()).deleteWorkflow(sourcePath),
  exportWorkflow: (_input: ExportWorkflowInput): Promise<string | null> =>
    Promise.reject(new Error(WORKFLOW_FILES_LOCAL_ONLY)),
  importWorkflowFile: (): Promise<ImportedWorkflowFile | null> =>
    Promise.reject(new Error(WORKFLOW_FILES_LOCAL_ONLY)),
  applyWorkflow: async (input: ApplyWorkflowInput): Promise<WorkflowRunReceipt> =>
    (await requireRemoteWorkflows()).applyWorkflow(input),
  undoWorkflowRun: async (runId: string): Promise<WorkflowUndoResult> =>
    (await requireRemoteWorkflows()).undoWorkflowRun(runId),
  listWorkflowRuns: async (): Promise<WorkflowRunSummary[]> =>
    (await client.supportsWorkflows()) ? await client.listWorkflowRuns() : [],
  deleteWorkflowRuns: async (workflowId: string): Promise<number> => {
    if (typeof workflowId !== 'string' || !workflowId) {
      throw new Error('deleteWorkflowRuns needs a workflow id')
    }
    return (await requireRemoteWorkflows()).deleteWorkflowRuns(workflowId)
  },

  // An older server has no template routes: it lists nothing, keeps the
  // built-in templates, and names the remedy on a write.
  listTemplates: async (): Promise<CustomTemplateFile[]> =>
    capabilities()?.supportsCustomTemplates ? await client.listTemplates() : [],
  readTemplate: async (sourcePath: string): Promise<string> =>
    requireRemoteTemplates('Editing a custom template').readTemplate(sourcePath),
  writeTemplate: async (input: WriteTemplateInput): Promise<CustomTemplateFile> =>
    requireRemoteTemplates('Saving a custom template').writeTemplate(input),
  deleteTemplate: async (sourcePath: string): Promise<void> =>
    requireRemoteTemplates('Deleting a custom template').deleteTemplate(sourcePath),

  getVaultTextSearchCapabilities: (): Promise<VaultTextSearchCapabilities> =>
    client.getVaultTextSearchCapabilities(),
  searchVaultText: (
    query: string,
    backend: VaultTextSearchBackendPreference = 'auto',
    paths: VaultTextSearchToolPaths = {}
  ): Promise<VaultTextSearchMatch[]> => client.searchVaultText(query, backend, paths),

  readNote: (relPath: string): Promise<NoteContent> => client.readNote(relPath),
  readNoteComments: (relPath: string): Promise<NoteComment[]> => client.readNoteComments(relPath),
  writeNoteComments: (relPath: string, comments: NoteCommentInput[]): Promise<NoteComment[]> =>
    client.writeNoteComments(relPath, comments),

  // A remote vault's notes are edited through its server, which records no
  // assistant edits, so there is nothing to review.
  listAgentEdits: async (): Promise<AgentEditSummary[]> => [],
  readAgentEdit: async (_relPath: string): Promise<AgentEditRecord | null> => null,
  writeAgentEdit: async (_relPath: string, _record: AgentEditRecord | null): Promise<void> => {},

  scanTasks: (): Promise<VaultTask[]> => client.scanTasks(),
  scanTasksForPath: (relPath: string): Promise<VaultTask[]> => client.scanTasksForPath(relPath),

  openDatabase: async (relPath: string): Promise<DatabaseDoc | null> => {
    try {
      return await databaseOpsForRemote().openDatabase(relPath)
    } catch (err) {
      // A tab can outlive its database (deleted here or by another client).
      if (err instanceof Error && err.message.startsWith('Database not found')) return null
      throw err
    }
  },
  writeDatabaseRows: (relPath: string, rows: DbRow[]): Promise<DatabaseDoc> =>
    databaseOpsForRemote().writeDatabaseRows(relPath, rows),
  writeDatabaseSchema: (
    relPath: string,
    sidecar: DatabaseSidecar,
    rows: DbRow[]
  ): Promise<DatabaseDoc> => databaseOpsForRemote().writeDatabaseSchema(relPath, sidecar, rows),
  createDatabase: (
    folder: NoteFolder,
    subpath: string,
    title?: string,
    seed?: DatabaseSeed
  ): Promise<DatabaseDoc> => databaseOpsForRemote().createDatabase(folder, subpath, title, seed),
  renameDatabase: (csvPath: string, newTitle: string): Promise<string> =>
    databaseOpsForRemote().renameDatabase(csvPath, newTitle),
  createRecordPage: (csvPath: string, title: string, body: string): Promise<string> =>
    databaseOpsForRemote().createRecordPage(csvPath, title, body),
  listDatabases: (): Promise<DatabaseSummary[]> => databaseOpsForRemote().listDatabases(),

  writeNote: (relPath: string, body: string): Promise<NoteMeta> => client.writeNote(relPath, body),
  // Servers have no append route: compose it from a read and a write.
  appendToNote: async (
    relPath: string,
    body: string,
    position: 'start' | 'end'
  ): Promise<NoteMeta> => {
    const safePosition = position === 'start' ? 'start' : 'end'
    const current = await client.readNote(relPath)
    const trimmed = body.replace(/\s+$/u, '')
    if (!trimmed) return current
    const next =
      safePosition === 'end'
        ? `${current.body}${current.body.endsWith('\n') ? '' : '\n'}\n${trimmed}\n`
        : `${trimmed}\n\n${current.body}`
    return client.writeNote(relPath, next)
  },
  createNote: (folder: NoteFolder, title?: string, subpath?: string): Promise<NoteMeta> =>
    client.createNote(folder, title, subpath ?? ''),
  createExcalidraw: (folder: NoteFolder, subpath?: string, title?: string): Promise<NoteMeta> =>
    client.createExcalidraw(folder, subpath ?? '', title),
  convertObsidianExcalidraw: (_relPath: string): Promise<NoteMeta> =>
    Promise.reject(new Error(OBSIDIAN_DRAWINGS_LOCAL_ONLY)),
  renameNote: (relPath: string, nextTitle: string): Promise<NoteMeta> =>
    client.renameNote(relPath, nextTitle),
  deleteNote: (relPath: string): Promise<void> => client.deleteNote(relPath),
  moveToTrash: (relPath: string): Promise<NoteMeta> => client.moveToTrash(relPath),
  restoreFromTrash: (relPath: string): Promise<NoteMeta> => client.restoreFromTrash(relPath),
  emptyTrash: (): Promise<void> => client.emptyTrash(),
  archiveNote: (relPath: string): Promise<NoteMeta> => client.archiveNote(relPath),
  unarchiveNote: (relPath: string): Promise<NoteMeta> => client.unarchiveNote(relPath),
  duplicateNote: (relPath: string): Promise<NoteMeta> => client.duplicateNote(relPath),
  moveNote: (relPath: string, targetFolder: NoteFolder, targetSubpath: string): Promise<NoteMeta> =>
    client.moveNote(relPath, targetFolder, targetSubpath),

  revealNote: (_relPath: string): Promise<void> => Promise.reject(new Error(REVEAL_LOCAL_ONLY)),
  revealNoteTarget: (_relPath: string): Promise<void> =>
    Promise.reject(new Error(REVEAL_LOCAL_ONLY)),
  revealFolder: (_folder: NoteFolder, _subpath: string): Promise<void> =>
    Promise.reject(new Error(REVEAL_LOCAL_ONLY)),
  revealFolderTarget: (_folder: NoteFolder, _subpath: string): Promise<void> =>
    Promise.reject(new Error(REVEAL_LOCAL_ONLY)),
  revealAssetsDir: (): Promise<void> => Promise.reject(new Error(REVEAL_LOCAL_ONLY)),

  // The asset lives on the server and the OS opener needs local bytes: Rust
  // downloads it into a temp folder of its own first.
  openAssetExternally: (relPath: string): Promise<{ ok: boolean; error?: string }> =>
    invoke('remote_open_asset_externally', { relPath }),

  // The dropped files are on this machine; the vault is not.
  importFilesToNote: (notePath: string, sourcePaths: string[]): Promise<ImportedAsset[]> =>
    client.uploadFiles(notePath, sourcePaths),
  importPastedImage: async (input: PastedImageInput): Promise<ImportedAsset> => {
    const data = input.data as ArrayBuffer | ArrayBufferView | undefined
    const bytes =
      data instanceof ArrayBuffer
        ? new Uint8Array(data)
        : ArrayBuffer.isView(data)
          ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
          : null
    if (!bytes) throw new Error('Clipboard image data is invalid.')
    if (bytes.byteLength === 0) throw new Error('Clipboard image is empty.')
    const filename = pastedImageFilename(input, new Date())
    const uploaded = await client.uploadAsset('', filename, bytes, input.mimeType)
    // A paste embeds as a wikilink exactly like the local path does; the
    // server's markdown is note-relative and meant for drag-drop imports.
    return {
      name: uploaded.name,
      path: uploaded.path,
      markdown: `![[${uploaded.path}]]`,
      kind: 'image' as const
    }
  },

  renameAsset: (relPath: string, nextName: string): Promise<AssetMeta> =>
    client.renameAsset(relPath, nextName),
  moveAsset: (relPath: string, targetDir: string): Promise<AssetMeta> =>
    client.moveAsset(relPath, targetDir),
  duplicateAsset: async (relPath: string): Promise<AssetMeta> =>
    requireRemoteAssetOps('Asset duplication').duplicateAsset(relPath),
  deleteAsset: async (relPath: string): Promise<DeletedAsset> =>
    requireRemoteAssetOps('Asset deletion').deleteAsset(relPath),
  restoreDeletedAsset: async (asset: DeletedAsset): Promise<AssetMeta> =>
    requireRemoteAssetOps('Asset restore').restoreDeletedAsset(asset),
  // Older servers have no deleted-assets store; an empty Trash view is the
  // truthful answer there, not an error.
  listDeletedAssets: async (): Promise<DeletedAsset[]> =>
    capabilities()?.supportsAssetOps ? await client.listDeletedAssets() : [],
  purgeDeletedAsset: async (undoToken: string): Promise<void> =>
    requireRemoteAssetOps('Asset deletion').purgeDeletedAsset(undoToken),
  emptyDeletedAssets: async (): Promise<void> =>
    requireRemoteAssetOps('Asset deletion').emptyDeletedAssets(),

  createFolder: (folder: NoteFolder, subpath: string): Promise<void> =>
    client.createFolder(folder, subpath),
  renameFolder: (folder: NoteFolder, oldSubpath: string, newSubpath: string): Promise<string> =>
    client.renameFolder(folder, oldSubpath, newSubpath),
  deleteFolder: (folder: NoteFolder, subpath: string): Promise<void> =>
    client.deleteFolder(folder, subpath),
  duplicateFolder: (folder: NoteFolder, subpath: string): Promise<string> =>
    client.duplicateFolder(folder, subpath)
}

export type RemoteVault = typeof remoteVault
