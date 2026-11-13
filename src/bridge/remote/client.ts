/**
 * The client for a ZenNotes server: a method-for-method port of upstream's
 * `RemoteServerClient` (apps/desktop/src/main/remote/server-client.ts), with
 * the same names, routes and error behaviour.
 *
 * Upstream's client calls `fetch` with the token in a header. Here every
 * request goes through Rust's `remote_request` (src-tauri/src/remote/http.rs),
 * which adds the token, so the token never enters the webview. The byte paths
 * (`uploadAsset`, `fetchAssetResponse`) and the change feed
 * (`watchVaultChanges`) are Rust commands of their own.
 *
 * Port-owned copy of upstream logic: on every re-vendor, diff
 * server-client.ts and carry each changed route here (see GAP-ANALYSIS.md,
 * "Remote workspaces"). `src/__tests__/remote-client.test.ts` holds the
 * route table this must match.
 */

import type {
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
  ServerCapabilities,
  VaultDemoTourResult,
  VaultInfo,
  VaultSettings,
  VaultTextSearchBackendPreference,
  VaultTextSearchCapabilities,
  VaultTextSearchMatch,
  VaultTextSearchToolPaths
} from '@zennotes/bridge-contract/ipc'
import type { CustomTemplateFile, WriteTemplateInput } from '@zennotes/bridge-contract/templates'
import type {
  ApplyWorkflowInput,
  WorkflowFile,
  WorkflowRunReceipt,
  WorkflowRunSummary,
  WorkflowUndoResult,
  WriteWorkflowInput
} from '@zennotes/bridge-contract/workflows'
import type { VaultTask } from '@zennotes/shared-domain/tasks'
import { prepareWorkflowRun } from '@zennotes/shared-domain/workflows/prepare-run'
import { stalePathMessage } from '@zennotes/shared-domain/remote-workspace-messages'
import { invoke } from '../ipc'
import { RemoteRequestError } from './errors'

export { RemoteConnectionError, RemoteRequestError } from './errors'

export interface RemoteServerClientOptions {
  /**
   * Called with the vault-relative path when the server answers 404 to a
   * request about it. The list this app shows is behind the server, so the
   * host re-pulls it (see `stalePathMessage`).
   */
  onStalePath?: (path: string) => void
}

interface JsonRequestInit {
  method?: 'GET' | 'POST'
  body?: unknown
}

export class RemoteServerClient {
  private readonly onStalePath: ((path: string) => void) | null

  constructor(options: RemoteServerClientOptions = {}) {
    this.onStalePath = options.onStalePath ?? null
  }

  async getCapabilities(): Promise<ServerCapabilities> {
    return this.jsonRequest<ServerCapabilities>('/api/capabilities')
  }

  async getCurrentVault(): Promise<VaultInfo | null> {
    return this.jsonRequest<VaultInfo | null>('/api/vault')
  }

  async getVaultSettings(): Promise<VaultSettings> {
    return this.jsonRequest<VaultSettings>('/api/vault/settings')
  }

  async setVaultSettings(next: VaultSettings): Promise<VaultSettings> {
    return this.jsonRequest<VaultSettings>('/api/vault/settings', {
      method: 'POST',
      body: next
    })
  }

  async selectVaultPath(path: string): Promise<VaultInfo> {
    return this.jsonRequest<VaultInfo>('/api/vault/select', {
      method: 'POST',
      body: { path }
    })
  }

  async browseDirectories(path = ''): Promise<DirectoryBrowseResult> {
    const query = path ? `?path=${encodeURIComponent(path)}` : ''
    return this.jsonRequest<DirectoryBrowseResult>(`/api/fs/browse${query}`)
  }

  async listNotes(): Promise<NoteMeta[]> {
    return this.jsonRequest<NoteMeta[]>('/api/notes')
  }

  async listFolders(): Promise<FolderEntry[]> {
    return this.jsonRequest<FolderEntry[]>('/api/folders')
  }

  async listAssets(): Promise<AssetMeta[]> {
    return this.jsonRequest<AssetMeta[]>('/api/assets')
  }

  async hasAssetsDir(): Promise<boolean> {
    return this.jsonRequest<{ exists: boolean }>('/api/assets/exists').then((resp) => resp.exists)
  }

  async generateDemoTour(): Promise<VaultDemoTourResult> {
    return this.jsonRequest<VaultDemoTourResult>('/api/demo/generate', { method: 'POST' })
  }

  async removeDemoTour(): Promise<VaultDemoTourResult> {
    return this.jsonRequest<VaultDemoTourResult>('/api/demo/remove', { method: 'POST' })
  }

  async getVaultTextSearchCapabilities(): Promise<VaultTextSearchCapabilities> {
    return this.jsonRequest<VaultTextSearchCapabilities>('/api/search/capabilities')
  }

  async searchVaultText(
    query: string,
    backend: VaultTextSearchBackendPreference = 'auto',
    paths: VaultTextSearchToolPaths = {}
  ): Promise<VaultTextSearchMatch[]> {
    const params = new URLSearchParams({ q: query, backend })
    if (paths.ripgrepPath) params.set('ripgrepPath', paths.ripgrepPath)
    if (paths.fzfPath) params.set('fzfPath', paths.fzfPath)
    return this.jsonRequest<VaultTextSearchMatch[]>(`/api/search/text?${params.toString()}`)
  }

  /** True when the connected server advertises the journalled workflow API
   *  from #608. Older servers stay read-only, exactly like the web client. */
  async supportsWorkflows(): Promise<boolean> {
    const caps = await this.getCapabilities()
    return (caps as { supportsWorkflows?: boolean } | null)?.supportsWorkflows === true
  }

  async listWorkflows(): Promise<WorkflowFile[]> {
    return this.jsonRequest<WorkflowFile[]>('/api/workflows')
  }

  async writeWorkflow(input: WriteWorkflowInput): Promise<WorkflowFile> {
    return this.jsonRequest<WorkflowFile>('/api/workflows/write', {
      method: 'POST',
      body: input
    })
  }

  async deleteWorkflow(sourcePath: string): Promise<void> {
    await this.jsonRequest('/api/workflows/delete', { method: 'POST', body: { sourcePath } })
  }

  /** True when the connected server advertises the custom-template routes
   *  from 2.46 (#723). */
  async supportsCustomTemplates(): Promise<boolean> {
    const caps = await this.getCapabilities()
    return caps?.supportsCustomTemplates === true
  }

  async listTemplates(): Promise<CustomTemplateFile[]> {
    return this.jsonRequest<CustomTemplateFile[]>('/api/templates')
  }

  async readTemplate(sourcePath: string): Promise<string> {
    const result = await this.jsonRequest<{ raw: string }>(
      `/api/templates/read?path=${encodeURIComponent(sourcePath)}`
    )
    return result.raw
  }

  async writeTemplate(input: WriteTemplateInput): Promise<CustomTemplateFile> {
    return this.jsonRequest<CustomTemplateFile>('/api/templates/write', {
      method: 'POST',
      body: input
    })
  }

  async deleteTemplate(sourcePath: string): Promise<void> {
    await this.jsonRequest('/api/templates/delete', { method: 'POST', body: { sourcePath } })
  }

  /** Prepare on this side (reads through the server), apply transactionally on
   *  the server — the same split the web bridge ships for #608. */
  async applyWorkflow(input: ApplyWorkflowInput): Promise<WorkflowRunReceipt> {
    const settings = await this.getVaultSettings()
    const prepared = await prepareWorkflowRun(input, {
      read: async (path: string) => {
        try {
          return (await this.readNote(path)).body
        } catch {
          return null
        }
      },
      systemFolderDirs: settings.systemFolderPaths ?? {}
    })
    return this.jsonRequest<WorkflowRunReceipt>('/api/workflows/apply', {
      method: 'POST',
      body: prepared
    })
  }

  async undoWorkflowRun(runId: string): Promise<WorkflowUndoResult> {
    return this.jsonRequest<WorkflowUndoResult>('/api/workflows/undo', {
      method: 'POST',
      body: { runId }
    })
  }

  async listWorkflowRuns(): Promise<WorkflowRunSummary[]> {
    return this.jsonRequest<WorkflowRunSummary[]>('/api/workflows/runs')
  }

  async deleteWorkflowRuns(workflowId: string): Promise<number> {
    return this.jsonRequest<number>('/api/workflows/runs/delete', {
      method: 'POST',
      body: { workflowId }
    })
  }

  async readNote(relPath: string): Promise<NoteContent> {
    return this.jsonRequest<NoteContent>(`/api/notes/read?path=${encodeURIComponent(relPath)}`)
  }

  async readNoteComments(relPath: string): Promise<NoteComment[]> {
    return this.jsonRequest<NoteComment[]>(`/api/comments/read?path=${encodeURIComponent(relPath)}`)
  }

  async writeNoteComments(relPath: string, comments: NoteCommentInput[]): Promise<NoteComment[]> {
    return this.jsonRequest<NoteComment[]>('/api/comments/write', {
      method: 'POST',
      body: { path: relPath, comments }
    })
  }

  async scanTasks(): Promise<VaultTask[]> {
    return this.jsonRequest<VaultTask[]>('/api/tasks')
  }

  async scanTasksForPath(relPath: string): Promise<VaultTask[]> {
    return this.jsonRequest<VaultTask[]>(`/api/tasks/for?path=${encodeURIComponent(relPath)}`)
  }

  async writeNote(relPath: string, body: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/write', {
      method: 'POST',
      body: { path: relPath, body }
    })
  }

  async createNote(folder: NoteFolder, title?: string, subpath = ''): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/create', {
      method: 'POST',
      body: { folder, title, subpath }
    })
  }

  async createExcalidraw(folder: NoteFolder, subpath = '', title?: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/excalidraw/create', {
      method: 'POST',
      body: { folder, subpath, title }
    })
  }

  async renameNote(relPath: string, nextTitle: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/rename', {
      method: 'POST',
      body: { path: relPath, title: nextTitle }
    })
  }

  async deleteNote(relPath: string): Promise<void> {
    await this.jsonRequest<void>('/api/notes/delete', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async moveToTrash(relPath: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/trash', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async restoreFromTrash(relPath: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/restore', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async emptyTrash(): Promise<void> {
    await this.jsonRequest<void>('/api/notes/empty-trash', { method: 'POST' })
  }

  async archiveNote(relPath: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/archive', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async unarchiveNote(relPath: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/unarchive', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async duplicateNote(relPath: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/duplicate', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async moveNote(relPath: string, targetFolder: NoteFolder, targetSubpath: string): Promise<NoteMeta> {
    return this.jsonRequest<NoteMeta>('/api/notes/move', {
      method: 'POST',
      body: { path: relPath, targetFolder, targetSubpath }
    })
  }

  async createFolder(folder: NoteFolder, subpath: string): Promise<void> {
    await this.jsonRequest<void>('/api/folders/create', {
      method: 'POST',
      body: { folder, subpath }
    })
  }

  async renameFolder(folder: NoteFolder, oldSubpath: string, newSubpath: string): Promise<string> {
    return this.jsonRequest<{ subpath: string }>('/api/folders/rename', {
      method: 'POST',
      body: { folder, oldSubpath, newSubpath }
    }).then((resp) => resp.subpath)
  }

  async deleteFolder(folder: NoteFolder, subpath: string): Promise<void> {
    await this.jsonRequest<void>('/api/folders/delete', {
      method: 'POST',
      body: { folder, subpath }
    })
  }

  async duplicateFolder(folder: NoteFolder, subpath: string): Promise<string> {
    return this.jsonRequest<{ subpath: string }>('/api/folders/duplicate', {
      method: 'POST',
      body: { folder, subpath }
    }).then((resp) => resp.subpath)
  }

  async renameAsset(relPath: string, nextName: string): Promise<AssetMeta> {
    return this.jsonRequest<AssetMeta>('/api/assets/rename', {
      method: 'POST',
      body: { path: relPath, name: nextName }
    })
  }

  async moveAsset(relPath: string, targetDir: string): Promise<AssetMeta> {
    return this.jsonRequest<AssetMeta>('/api/assets/move', {
      method: 'POST',
      body: { path: relPath, targetDir }
    })
  }

  async duplicateAsset(relPath: string): Promise<AssetMeta> {
    return this.jsonRequest<AssetMeta>('/api/assets/duplicate', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async deleteAsset(relPath: string): Promise<DeletedAsset> {
    return this.jsonRequest<DeletedAsset>('/api/assets/delete', {
      method: 'POST',
      body: { path: relPath }
    })
  }

  async listDeletedAssets(): Promise<DeletedAsset[]> {
    return this.jsonRequest<DeletedAsset[]>('/api/assets/deleted')
  }

  async restoreDeletedAsset(deleted: DeletedAsset): Promise<AssetMeta> {
    return this.jsonRequest<AssetMeta>('/api/assets/restore', {
      method: 'POST',
      body: deleted
    })
  }

  async purgeDeletedAsset(undoToken: string): Promise<void> {
    await this.jsonRequest<void>('/api/assets/purge', {
      method: 'POST',
      body: { undoToken }
    })
  }

  async emptyDeletedAssets(): Promise<void> {
    await this.jsonRequest<void>('/api/assets/empty-deleted', { method: 'POST' })
  }

  /** `uploadAsset` for files on this machine (a drop): Rust streams each one
   *  from disk to `/api/assets/upload`, collecting failures per file. */
  async uploadFiles(notePath: string, sourcePaths: string[]): Promise<ImportedAsset[]> {
    return invoke<ImportedAsset[]>('remote_upload_files', { notePath, sourcePaths })
  }

  /** `uploadAsset` for bytes the webview holds (a paste). */
  async uploadAsset(
    notePath: string,
    filename: string,
    bytes: Uint8Array,
    mimeType = 'application/octet-stream'
  ): Promise<ImportedAsset> {
    return invoke<ImportedAsset>('remote_upload_bytes', {
      notePath,
      fileName: filename,
      mimeType,
      data: Array.from(bytes)
    })
  }

  /** The asset's bytes, base64 (upstream reads `fetchAssetResponse`). */
  async readAssetBase64(assetPath: string): Promise<string> {
    return invoke<string>('remote_read_asset_base64', { assetPath })
  }

  private async jsonRequest<T>(path: string, init: JsonRequestInit = {}): Promise<T> {
    try {
      return await invoke<T>('remote_request', {
        method: init.method ?? 'GET',
        path,
        body: init.body === undefined ? null : init.body
      })
    } catch (error) {
      // A 404 for a path this app asked to change means the list is behind
      // the server, not that the server is broken: another device moved,
      // renamed, or trashed the note and the change never arrived here
      // (#734). Say which path is gone and have the host re-pull the list.
      // Reads keep the plain answer: a 404 on `?path=` is how remote
      // databases learn a file is absent (#556), and that is not staleness.
      if (error instanceof RemoteRequestError && error.status === 404) {
        const stalePath = requestedPath(init.body)
        if (stalePath !== null) {
          this.onStalePath?.(stalePath)
          throw new RemoteRequestError(stalePathMessage(stalePath), error.status)
        }
      }
      throw error
    }
  }
}

/** The vault-relative path a JSON request body names, when it names one. */
function requestedPath(body: unknown): string | null {
  if (!body || typeof body !== 'object') return null
  const path = (body as { path?: unknown }).path
  return typeof path === 'string' && path.length > 0 ? path : null
}
