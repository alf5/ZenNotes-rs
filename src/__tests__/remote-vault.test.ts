/**
 * The remote branch of the vault bridge (src/bridge/remote/vault.ts): what
 * upstream's main process does under `isRemoteWorkspaceActive()`, and that
 * tauri-bridge.ts sends every vault member through it.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { RemoteWorkspaceInfo, ServerCapabilities } from '@zennotes/bridge-contract/ipc'

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
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ label: 'main' }) }))

const { remoteVault, REVEAL_LOCAL_ONLY, WORKFLOW_FILES_LOCAL_ONLY } = await import('../bridge/remote/vault')
const { setRemoteWorkspaceInfoForTest, remoteConnected } = await import('../bridge/remote/state')

function connectedWith(caps: Partial<ServerCapabilities>): void {
  const info: RemoteWorkspaceInfo = {
    mode: 'remote',
    baseUrl: 'https://zen.example',
    authConfigured: true,
    capabilities: {
      version: '2.79.0',
      platform: 'linux',
      authRequired: true,
      supportsSessionLogin: true,
      browseRootsEnforced: true,
      supportsVaultSelection: true,
      supportsDirectoryBrowsing: true,
      supportsWatch: true,
      ...caps
    },
    profileId: 'p1',
    bootError: null
  }
  setRemoteWorkspaceInfoForTest(info)
}

const paths = () => calls.filter((c) => c.command === 'remote_request').map((c) => c.args.path)

beforeEach(() => {
  calls.length = 0
  reply = () => null
  connectedWith({})
})

describe('a remote workspace', () => {
  it('is connected only with capabilities; a failed boot is configured, not connected', () => {
    expect(remoteConnected()).toBe(true)
    setRemoteWorkspaceInfoForTest({
      mode: 'remote',
      baseUrl: 'https://zen.example',
      authConfigured: false,
      capabilities: null,
      profileId: 'p1',
      bootError: 'Could not connect'
    })
    expect(remoteConnected()).toBe(false)
    setRemoteWorkspaceInfoForTest(null)
    expect(remoteConnected()).toBe(false)
  })

  it('refuses what only a local vault can do, in upstream’s words', async () => {
    await expect(remoteVault.revealNote('a.md')).rejects.toThrow(REVEAL_LOCAL_ONLY)
    await expect(remoteVault.revealAssetsDir()).rejects.toThrow(REVEAL_LOCAL_ONLY)
    await expect(remoteVault.exportWorkflow({ suggestedName: 'x', raw: '' })).rejects.toThrow(
      WORKFLOW_FILES_LOCAL_ONLY
    )
    await expect(remoteVault.importWorkflowFile()).rejects.toThrow(WORKFLOW_FILES_LOCAL_ONLY)
    expect(calls).toEqual([])
  })

  it('keeps no workspace state or agent edits', async () => {
    await expect(remoteVault.readWorkspaceState()).resolves.toBeNull()
    await expect(remoteVault.listAgentEdits()).resolves.toEqual([])
    await expect(remoteVault.readAgentEdit('a.md')).resolves.toBeNull()
    await expect(remoteVault.rootContentHiddenByInboxMode()).resolves.toBe(false)
    expect(calls).toEqual([])
  })

  it('names the server update an older server needs', async () => {
    connectedWith({ supportsCustomTemplates: false, supportsAssetOps: false })
    await expect(remoteVault.listTemplates()).resolves.toEqual([])
    await expect(remoteVault.writeTemplate({ raw: '' } as never)).rejects.toThrow(
      'Saving a custom template needs ZenNotes server 2.46 or later. Update the server and reconnect this workspace.'
    )
    await expect(remoteVault.listDeletedAssets()).resolves.toEqual([])
    await expect(remoteVault.deleteAsset('assets/a.png')).rejects.toThrow(
      'Asset deletion needs a newer ZenNotes server. Update the server and reconnect this workspace.'
    )
    expect(paths()).toEqual([])

    connectedWith({ supportsCustomTemplates: true, supportsAssetOps: true })
    reply = () => []
    await remoteVault.listTemplates()
    await remoteVault.listDeletedAssets()
    expect(paths()).toEqual(['/api/templates', '/api/assets/deleted'])
  })

  it('lists no workflows on a server without them, and refuses to write one', async () => {
    reply = () => ({ supportsWorkflows: false })
    await expect(remoteVault.listWorkflows()).resolves.toEqual([])
    await expect(remoteVault.writeWorkflow({ slug: 's', raw: '' })).rejects.toThrow(
      'This ZenNotes server does not support workflows yet. Update the server and reconnect.'
    )
    expect(paths()).toEqual(['/api/capabilities', '/api/capabilities'])
  })

  it('appends with a read and a write, as upstream composes it', async () => {
    reply = ({ args }) =>
      args.method === 'GET' ? { path: 'inbox/a.md', body: 'first' } : { path: 'inbox/a.md' }
    await remoteVault.appendToNote('inbox/a.md', 'second  \n', 'end')
    await remoteVault.appendToNote('inbox/a.md', 'zeroth', 'start')
    const writes = calls.filter((c) => c.args.method === 'POST').map((c) => (c.args.body as { body: string }).body)
    expect(writes).toEqual(['first\n\nsecond\n', 'zeroth\n\nfirst'])
  })

  it('uploads a pasted image and embeds it as a wikilink', async () => {
    reply = () => ({ name: 'Pasted image.png', path: 'assets/Pasted image.png', markdown: '![](x)', kind: 'image' })
    const result = await remoteVault.importPastedImage({
      data: new Uint8Array([137, 80]).buffer,
      mimeType: 'image/png'
    })
    expect(result.markdown).toBe('![[assets/Pasted image.png]]')
    expect(calls[0]!.command).toBe('remote_upload_bytes')
    expect(calls[0]!.args.notePath).toBe('')
    await expect(
      remoteVault.importPastedImage({ data: new ArrayBuffer(0), mimeType: 'image/png' })
    ).rejects.toThrow('Clipboard image is empty.')
  })

  it('composes databases from the note endpoints, and forgets one that is gone', async () => {
    connectedWith({ reportsMissingAsNotFound: true })
    reply = ({ args }) => {
      if (args.path === '/api/vault/settings') return { primaryNotesLocation: 'inbox' }
      throw { remoteError: 'request', status: 404, message: 'not found' }
    }
    await expect(remoteVault.openDatabase('inbox/Books.base/data.csv')).resolves.toBeNull()
  })
})

describe('tauri-bridge.ts', () => {
  const source = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), '..', 'bridge', 'tauri-bridge.ts'),
    'utf8'
  )

  /** Members that reach the local disk but are not vault operations: they
   *  act on this machine (a file outside the vault, the config file, a link
   *  preview) or switch the workspace itself. */
  const LOCAL_BY_DESIGN = new Set([
    'getCurrentVault',
    'listLocalVaults',
    'openLocalVault',
    'closeVault',
    'pickVault',
    'selectVaultPath',
    'revealConfigFile',
    'revealFilePath',
    'openVaultInFileManager',
    'openExternalFile',
    'fetchLinkMetadata'
  ])

  it('routes every vault member through the server while a session is on', () => {
    const offenders: string[] = []
    const lines = source.split('\n')
    lines.forEach((line, i) => {
      if (/^\s*\/\//.test(line)) return
      if (!/invoke\('(vault|db|workspace_state)_/.test(line) && !/\b(databases|tasks|workflows|agentEdits)\./.test(line)) return
      // The member this line belongs to: the nearest `  name:` above it.
      let j = i
      while (j >= 0 && !/^ {2}[a-zA-Z]+: /.test(lines[j]!)) j--
      const member = lines[j]?.match(/^ {2}([a-zA-Z]+):/)?.[1]
      if (!member || LOCAL_BY_DESIGN.has(member)) return
      const text = lines.slice(j, i + 1).join('').replace(/\s+/g, '')
      if (!text.includes(`routed(remoteVault.${member},`)) offenders.push(member)
    })
    expect([...new Set(offenders)]).toEqual([])
  })
})
