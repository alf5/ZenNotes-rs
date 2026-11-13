/**
 * The webview's server client (src/bridge/remote/client.ts) against upstream's
 * `RemoteServerClient` (apps/desktop/src/main/remote/server-client.ts).
 *
 * ROUTES is upstream's route table, copied by hand. On a re-vendor, diff
 * server-client.ts and update this table first: a route upstream changed
 * then fails here until client.ts follows.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'

type Call = { command: string; args: Record<string, unknown> }
const calls: Call[] = []
let reply: (call: Call) => unknown = () => null

vi.mock('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    const call = { command, args }
    calls.push(call)
    return reply(call)
  }
}))

const { RemoteServerClient, RemoteConnectionError, RemoteRequestError } = await import(
  '../bridge/remote/client'
)
const { stalePathMessage } = await import('@zennotes/shared-domain/remote-workspace-messages')

type Client = InstanceType<typeof RemoteServerClient>

/** [method, args, HTTP method, path, JSON body] as upstream sends them. */
const ROUTES: Array<[keyof Client, unknown[], 'GET' | 'POST', string, unknown]> = [
  ['getCapabilities', [], 'GET', '/api/capabilities', null],
  ['getCurrentVault', [], 'GET', '/api/vault', null],
  ['getVaultSettings', [], 'GET', '/api/vault/settings', null],
  ['setVaultSettings', [{ primaryNotesLocation: 'root' }], 'POST', '/api/vault/settings', { primaryNotesLocation: 'root' }],
  ['selectVaultPath', ['/My vaults/A'], 'POST', '/api/vault/select', { path: '/My vaults/A' }],
  ['browseDirectories', [], 'GET', '/api/fs/browse', null],
  ['browseDirectories', ['/My vaults'], 'GET', '/api/fs/browse?path=%2FMy%20vaults', null],
  ['listNotes', [], 'GET', '/api/notes', null],
  ['listFolders', [], 'GET', '/api/folders', null],
  ['listAssets', [], 'GET', '/api/assets', null],
  ['generateDemoTour', [], 'POST', '/api/demo/generate', null],
  ['removeDemoTour', [], 'POST', '/api/demo/remove', null],
  ['getVaultTextSearchCapabilities', [], 'GET', '/api/search/capabilities', null],
  ['searchVaultText', ['a b', 'ripgrep', { ripgrepPath: '/rg' }], 'GET', '/api/search/text?q=a+b&backend=ripgrep&ripgrepPath=%2Frg', null],
  ['listWorkflows', [], 'GET', '/api/workflows', null],
  ['writeWorkflow', [{ slug: 's', raw: 'r' }], 'POST', '/api/workflows/write', { slug: 's', raw: 'r' }],
  ['deleteWorkflow', ['w.md'], 'POST', '/api/workflows/delete', { sourcePath: 'w.md' }],
  ['listTemplates', [], 'GET', '/api/templates', null],
  ['writeTemplate', [{ raw: 'x' }], 'POST', '/api/templates/write', { raw: 'x' }],
  ['deleteTemplate', ['t.md'], 'POST', '/api/templates/delete', { sourcePath: 't.md' }],
  ['undoWorkflowRun', ['run1'], 'POST', '/api/workflows/undo', { runId: 'run1' }],
  ['listWorkflowRuns', [], 'GET', '/api/workflows/runs', null],
  ['deleteWorkflowRuns', ['wf'], 'POST', '/api/workflows/runs/delete', { workflowId: 'wf' }],
  ['readNote', ['inbox/a b.md'], 'GET', '/api/notes/read?path=inbox%2Fa%20b.md', null],
  ['readNoteComments', ['inbox/a.md'], 'GET', '/api/comments/read?path=inbox%2Fa.md', null],
  ['writeNoteComments', ['inbox/a.md', []], 'POST', '/api/comments/write', { path: 'inbox/a.md', comments: [] }],
  ['scanTasks', [], 'GET', '/api/tasks', null],
  ['scanTasksForPath', ['inbox/a.md'], 'GET', '/api/tasks/for?path=inbox%2Fa.md', null],
  ['writeNote', ['inbox/a.md', 'hi'], 'POST', '/api/notes/write', { path: 'inbox/a.md', body: 'hi' }],
  ['createNote', ['inbox', 'T'], 'POST', '/api/notes/create', { folder: 'inbox', title: 'T', subpath: '' }],
  ['createExcalidraw', ['inbox', 'sub', 'D'], 'POST', '/api/excalidraw/create', { folder: 'inbox', subpath: 'sub', title: 'D' }],
  ['renameNote', ['inbox/a.md', 'B'], 'POST', '/api/notes/rename', { path: 'inbox/a.md', title: 'B' }],
  ['deleteNote', ['inbox/a.md'], 'POST', '/api/notes/delete', { path: 'inbox/a.md' }],
  ['moveToTrash', ['inbox/a.md'], 'POST', '/api/notes/trash', { path: 'inbox/a.md' }],
  ['restoreFromTrash', ['trash/a.md'], 'POST', '/api/notes/restore', { path: 'trash/a.md' }],
  ['emptyTrash', [], 'POST', '/api/notes/empty-trash', null],
  ['archiveNote', ['inbox/a.md'], 'POST', '/api/notes/archive', { path: 'inbox/a.md' }],
  ['unarchiveNote', ['archive/a.md'], 'POST', '/api/notes/unarchive', { path: 'archive/a.md' }],
  ['duplicateNote', ['inbox/a.md'], 'POST', '/api/notes/duplicate', { path: 'inbox/a.md' }],
  ['moveNote', ['inbox/a.md', 'archive', 'x'], 'POST', '/api/notes/move', { path: 'inbox/a.md', targetFolder: 'archive', targetSubpath: 'x' }],
  ['createFolder', ['inbox', 'x'], 'POST', '/api/folders/create', { folder: 'inbox', subpath: 'x' }],
  ['deleteFolder', ['inbox', 'x'], 'POST', '/api/folders/delete', { folder: 'inbox', subpath: 'x' }],
  ['renameAsset', ['assets/a.png', 'b.png'], 'POST', '/api/assets/rename', { path: 'assets/a.png', name: 'b.png' }],
  ['moveAsset', ['assets/a.png', 'assets/x'], 'POST', '/api/assets/move', { path: 'assets/a.png', targetDir: 'assets/x' }],
  ['duplicateAsset', ['assets/a.png'], 'POST', '/api/assets/duplicate', { path: 'assets/a.png' }],
  ['deleteAsset', ['assets/a.png'], 'POST', '/api/assets/delete', { path: 'assets/a.png' }],
  ['listDeletedAssets', [], 'GET', '/api/assets/deleted', null],
  ['restoreDeletedAsset', [{ undoToken: 'u' }], 'POST', '/api/assets/restore', { undoToken: 'u' }],
  ['purgeDeletedAsset', ['u'], 'POST', '/api/assets/purge', { undoToken: 'u' }],
  ['emptyDeletedAssets', [], 'POST', '/api/assets/empty-deleted', null]
]

beforeEach(() => {
  calls.length = 0
  reply = () => null
})

describe('the remote client', () => {
  it.each(ROUTES)('%s goes to upstream’s route', async (method, args, http, path, body) => {
    const client = new RemoteServerClient()
    await (client[method] as (...a: unknown[]) => Promise<unknown>)(...args)
    expect(calls).toEqual([{ command: 'remote_request', args: { method: http, path, body } }])
  })

  it('unwraps the answers upstream unwraps', async () => {
    const client = new RemoteServerClient()
    reply = ({ args }) =>
      args.path === '/api/assets/exists'
        ? { exists: true }
        : args.path === '/api/templates/read?path=t.md'
          ? { raw: 'body' }
          : { subpath: 'new' }
    await expect(client.hasAssetsDir()).resolves.toBe(true)
    await expect(client.readTemplate('t.md')).resolves.toBe('body')
    await expect(client.renameFolder('inbox', 'a', 'b')).resolves.toBe('new')
    await expect(client.duplicateFolder('inbox', 'a')).resolves.toBe('new')
    expect(calls.map((c) => c.args.path)).toEqual([
      '/api/assets/exists',
      '/api/templates/read?path=t.md',
      '/api/folders/rename',
      '/api/folders/duplicate'
    ])
  })

  it('asks the server whether it has workflows and templates', async () => {
    const client = new RemoteServerClient()
    reply = () => ({ supportsWorkflows: true })
    await expect(client.supportsWorkflows()).resolves.toBe(true)
    await expect(client.supportsCustomTemplates()).resolves.toBe(false)
    expect(calls.map((c) => c.args.path)).toEqual(['/api/capabilities', '/api/capabilities'])
  })

  it('says a path another device moved is gone, and asks for a resync', async () => {
    const stale = vi.fn()
    const client = new RemoteServerClient({ onStalePath: stale })
    reply = () => {
      throw { remoteError: 'request', status: 404, message: 'Remote server request failed (404 Not Found)' }
    }
    const err = await client.writeNote('inbox/gone.md', 'x').catch((e: unknown) => e)
    expect(err).toBeInstanceOf(RemoteRequestError)
    expect((err as InstanceType<typeof RemoteRequestError>).status).toBe(404)
    expect((err as Error).message).toBe(stalePathMessage('inbox/gone.md'))
    expect(stale).toHaveBeenCalledWith('inbox/gone.md')
  })

  it('leaves a 404 on a read alone: databases read it as absence', async () => {
    const stale = vi.fn()
    const client = new RemoteServerClient({ onStalePath: stale })
    reply = () => {
      throw { remoteError: 'request', status: 404, message: 'plain' }
    }
    const err = await client.readNote('x.base/schema.json').catch((e: unknown) => e)
    expect(err).toBeInstanceOf(RemoteRequestError)
    expect((err as Error).message).toBe('plain')
    expect(stale).not.toHaveBeenCalled()
  })

  it('keeps a dead server apart from a refusal', async () => {
    const client = new RemoteServerClient()
    reply = () => {
      throw { remoteError: 'connection', message: 'Could not connect to the ZenNotes server at https://x.' }
    }
    const err = await client.listNotes().catch((e: unknown) => e)
    expect(err).toBeInstanceOf(RemoteConnectionError)
    expect((err as Error).message).toBe('Could not connect to the ZenNotes server at https://x.')

    reply = () => {
      throw { remoteError: 'request', status: 401, message: 'The ZenNotes server rejected the connection.' }
    }
    const refused = await client.listNotes().catch((e: unknown) => e)
    expect(refused).toBeInstanceOf(RemoteRequestError)
    expect((refused as InstanceType<typeof RemoteRequestError>).status).toBe(401)
  })

  it('hands bytes to Rust, never a token', async () => {
    const client = new RemoteServerClient()
    reply = () => ({ name: 'p.png', path: 'assets/p.png' })
    await client.uploadAsset('', 'p.png', new Uint8Array([1, 2, 3]), 'image/png')
    await client.uploadFiles('inbox/a.md', ['/home/me/x.pdf'])
    expect(calls).toEqual([
      {
        command: 'remote_upload_bytes',
        args: { notePath: '', fileName: 'p.png', mimeType: 'image/png', data: [1, 2, 3] }
      },
      { command: 'remote_upload_files', args: { notePath: 'inbox/a.md', sourcePaths: ['/home/me/x.pdf'] } }
    ])
  })
})
