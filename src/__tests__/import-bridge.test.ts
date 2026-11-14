/**
 * The import members of the bridge (src/bridge/imports): the controllers
 * against a Node stand-in for the Rust host, the Notion fetches that keep the
 * credential on the host, and the path helpers the run code relies on.
 */
import { promises as fs } from 'node:fs'
import os from 'node:os'
import nodePath from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { NotionImportProgress } from '@zennotes/bridge-contract/notion-import'
import { createImportHost, type ImportHost } from './helpers/import-host'

let host: ImportHost
const listeners = new Map<string, Array<(event: { payload: unknown }) => void>>()
const emit = (event: string, payload: unknown): void => {
  for (const listener of listeners.get(event) ?? []) listener({ payload })
}

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args: Record<string, unknown> = {}) => host.invoke(command, args)
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (event: string, cb: (event: { payload: unknown }) => void) => {
    listeners.set(event, [...(listeners.get(event) ?? []), cb])
    return () => listeners.set(event, (listeners.get(event) ?? []).filter((other) => other !== cb))
  }
}))
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0-test' }))

const path = await import('../bridge/imports/path')
const { importMembers } = await import('../bridge/imports')
const { HELD, apiFetch, mcpFetch, refreshSignIn } = await import('../bridge/imports/notion-host')
const { TAURI_CAPABILITIES } = await import('../bridge/capabilities')
const { PAGE } = await import('../bridge/imports/host')

const until = async (check: () => boolean): Promise<void> => {
  for (let i = 0; i < 400 && !check(); i++) await new Promise((r) => setTimeout(r, 5))
  expect(check()).toBe(true)
}

describe('path helpers', () => {
  it('agree with node:path.posix on the paths the run code makes', () => {
    const joins: string[][] = [
      ['/obsidian/staging', 'out'],
      ['/obsidian/vault/inbox', 'Obsidian Import'],
      ['/a/b', '../c'],
      ['/a', 'b/', './c'],
      ['/a', ''],
      ['a', 'b']
    ]
    for (const parts of joins) expect(path.join(...parts)).toBe(nodePath.posix.join(...parts))
    for (const p of ['/a/b/c.md', '/a/b/', '/a', '/', 'a', 'a/b', 'Logseq', '/obsidian/vault/inbox/Logseq 2']) {
      expect(path.dirname(p)).toBe(nodePath.posix.dirname(p))
      expect(path.basename(p)).toBe(nodePath.posix.basename(p))
    }
    expect(path.basename('/x/Note.md', '.md')).toBe('Note')
    expect(path.basename('/x/.md', '.md')).toBe(nodePath.posix.basename('/x/.md', '.md'))
    const pairs: Array<[string, string]> = [
      ['/logseq/vault', '/logseq/vault/inbox/Logseq'],
      ['/logseq/vault/inbox', '/logseq/vault'],
      ['/logseq/vault', '/logseq/vault'],
      ['/logseq/source', '/logseq/vault'],
      ['/', '/a/b']
    ]
    for (const [from, to] of pairs) expect(path.relative(from, to)).toBe(nodePath.posix.relative(from, to))
    expect(path.resolve('/logseq/vault', 'inbox/../../escape/x.md')).toBe('/logseq/escape/x.md')
    expect(path.resolve('/a', '/b', 'c')).toBe('/b/c')
  })
})

describe('the bridge', () => {
  it('offers every import member, and says imports are supported', () => {
    expect(TAURI_CAPABILITIES.supportsImport).toBe(true)
    expect(Object.keys(importMembers)).toHaveLength(23)
    for (const value of Object.values(importMembers)) expect(typeof value).toBe('function')
  })
})

describe('the Obsidian controller', () => {
  let scratch: string
  let source: string
  let root: string
  const reports: NotionImportProgress[] = []

  beforeEach(async () => {
    scratch = await fs.mkdtemp(nodePath.join(os.tmpdir(), 'zen-import-bridge-'))
    source = nodePath.join(scratch, 'Vault')
    root = nodePath.join(scratch, 'vault')
    await fs.mkdir(nodePath.join(source, 'Notes'), { recursive: true })
    await fs.mkdir(root)
    for (let i = 0; i < 5; i++) await fs.writeFile(nodePath.join(source, 'Notes', `n${i}.md`), `note ${i}\n`)
    host = createImportHost({
      '/obsidian/source': source,
      '/obsidian/picked': source,
      '/obsidian/vault': root,
      '/obsidian/staging': nodePath.join(scratch, 'staging')
    })
    reports.length = 0
    host.handlers.import_begin = (args) => {
      expect(args).toEqual({ kind: 'obsidian', page: PAGE })
      return {
        root: '/obsidian/vault',
        source: '/obsidian/source',
        stagingDir: '/obsidian/staging',
        notesRoot: '/obsidian/vault',
        existingNotes: [],
        existingAssets: []
      }
    }
    host.handlers.import_status = () => ({ running: true, progress: null, stopping: false })
    host.handlers.import_report = (args) => {
      expect(args.kind).toBe('obsidian')
      expect(args.page).toBe(PAGE)
      reports.push(args.progress as NotionImportProgress)
    }
  })

  afterEach(async () => {
    await fs.rm(scratch, { recursive: true, force: true })
  })

  it('runs in the background and reports the result last', async () => {
    await importMembers.obsidianImportStart({ includeTasks: true, excluded: [] })
    await until(() => reports.at(-1)?.phase === 'done')
    expect(reports[0].phase).toBe('finding')
    const done = reports.at(-1)
    expect(done?.result?.folder).toBe('Obsidian Import')
    expect(done?.result?.pages).toBe(5)
    expect(await fs.readFile(nodePath.join(root, 'Obsidian Import/Notes/n3.md'), 'utf8')).toBe('note 3\n')
    // Spaced out as upstream's broadcast is: not one report per file.
    expect(reports.length).toBeLessThan(8)
  })

  it('stops when any window presses Stop, and leaves the vault alone', async () => {
    const real = host.invoke
    let copies = 0
    host.invoke = async (command, args) => {
      // Stop pressed partway: the host tells every page, and the one running
      // the import aborts.
      if (command === 'import_fs_copy' && ++copies === 2) emit('import://stop', 'obsidian')
      return real(command, args)
    }
    await importMembers.obsidianImportStart({ includeTasks: true, excluded: [] })
    await until(() => ['stopped', 'done', 'failed'].includes(reports.at(-1)?.phase ?? ''))
    expect(reports.at(-1)?.phase).toBe('stopped')
    expect(await fs.readdir(root)).toEqual([])
  })

  it('does not miss a Stop pressed before the page was listening for one', async () => {
    host.handlers.import_status = () => ({ running: true, progress: null, stopping: true })
    host.handlers.import_report = (args) => {
      reports.push(args.progress as NotionImportProgress)
    }
    await importMembers.obsidianImportStart({ includeTasks: true, excluded: [] })
    await until(() => ['stopped', 'done', 'failed'].includes(reports.at(-1)?.phase ?? ''))
    expect(reports.at(-1)?.phase).toBe('stopped')
    // What status tells the modal leaves out the host's own flag.
    await expect(importMembers.obsidianImportStatus()).resolves.toEqual({ running: true, progress: null })
  })

  it('passes a refusal from the host on as an Error', async () => {
    host.handlers.import_begin = () => {
      throw 'Imports write to a local vault.'
    }
    await expect(importMembers.obsidianImportStart({ includeTasks: true, excluded: [] })).rejects.toThrow(
      'Imports write to a local vault.'
    )
  })

  it('hears progress for its own import only', () => {
    const heard: NotionImportProgress[] = []
    const stop = importMembers.onObsidianImportProgress((progress) => heard.push(progress))
    return new Promise<void>((resolve) => setTimeout(resolve, 0)).then(() => {
      emit('import://obsidian-progress', { phase: 'writing', done: 1, total: 2 })
      emit('import://notion-progress', { phase: 'writing', done: 9, total: 9 })
      stop()
      emit('import://obsidian-progress', { phase: 'done', done: 1, total: 1 })
      expect(heard).toEqual([{ phase: 'writing', done: 1, total: 2 }])
    })
  })
})

describe('Notion', () => {
  beforeEach(() => {
    host = createImportHost({})
  })

  it('needs a sign-in or a token to start, and gives the run back', async () => {
    host.handlers.import_begin = () => ({
      root: '/notion/vault',
      source: '/notion/source',
      stagingDir: '/notion/staging',
      notesRoot: '/notion/vault',
      existingNotes: [],
      existingAssets: []
    })
    host.handlers.notion_credentials = () => ({ signedIn: false, tokenSet: false })
    host.handlers.import_end = () => undefined
    await expect(importMembers.notionImportStart({ includeTasks: false, excluded: [] })).rejects.toThrow(
      'Sign in to Notion first.'
    )
    expect(host.calls.find((call) => call.command === 'import_end')?.args).toEqual({ kind: 'notion', page: PAGE })
  })

  it('checks a pasted token with Notion before the host keeps it', async () => {
    const seen: Array<Record<string, unknown>> = []
    host.handlers.notion_api_request = (args) => {
      seen.push(args)
      return { status: 200, headers: {}, body: JSON.stringify({ bot: { workspace_name: 'W' } }) }
    }
    host.handlers.notion_token_set = () => true
    await expect(importMembers.notionImportSaveToken('  secret_abc  ')).resolves.toEqual({ ok: true })
    expect(seen[0].url).toBe('https://api.notion.com/v1/users/me')
    expect(seen[0].token).toBe('secret_abc')
    expect(Object.keys(seen[0].headers as object).map((h) => h.toLowerCase())).not.toContain('authorization')
    expect(host.calls.find((call) => call.command === 'notion_token_set')?.args).toEqual({ token: 'secret_abc' })

    host.handlers.notion_api_request = () => ({ status: 401, headers: {}, body: '{"code":"unauthorized"}' })
    await expect(importMembers.notionImportSaveToken('bad')).resolves.toEqual({
      ok: false,
      error: 'Notion refused this token. Check it and save it again.'
    })
    await expect(importMembers.notionImportSaveToken('   ')).resolves.toEqual({ ok: false, error: 'Paste a token first.' })
  })

  it('reads the workspace with the token the host holds', async () => {
    host.handlers.notion_credentials = () => ({ signedIn: false, tokenSet: true })
    const tokens: unknown[] = []
    host.handlers.notion_api_request = (args) => {
      tokens.push(args.token)
      if (String(args.url).endsWith('/v1/users/me')) {
        return { status: 200, headers: {}, body: JSON.stringify({ bot: { workspace_name: 'Team' } }) }
      }
      return {
        status: 200,
        headers: {},
        body: JSON.stringify({
          results: [
            {
              object: 'page',
              id: 'aaaaaaaa-0000-4000-8000-000000000000',
              parent: { type: 'workspace', workspace: true },
              properties: { title: { id: 'title', type: 'title', title: [{ type: 'text', plain_text: 'Work' }] } }
            }
          ],
          has_more: false
        })
      }
    }
    const tree = await importMembers.notionImportTree()
    expect(tree.workspace).toBe('Team')
    expect(tree.items).toEqual([{ id: 'aaaaaaaa000040008000000000000000', title: 'Work', kind: 'page' }])
    // The client only ever held the stand-in; the host attaches the token.
    expect(tokens).toEqual([null, null])
  })

  it('refuses a page id that is not one', async () => {
    await expect(importMembers.notionImportTreeChildren('../../etc')).rejects.toThrow('Not a page.')
  })

  it('sends MCP calls without the client’s authorization, and refreshes on the host', async () => {
    host.handlers.notion_mcp_request = (args) => {
      expect(args.headers).toEqual({ 'mcp-protocol-version': '2025-06-18' })
      return { status: 200, headers: { 'mcp-session-id': 's-1' }, body: 'data' }
    }
    const reply = await mcpFetch('https://mcp.notion.com/mcp', {
      method: 'POST',
      headers: { authorization: `Bearer ${HELD}`, 'mcp-protocol-version': '2025-06-18' },
      body: '{}'
    })
    expect(reply.headers.get('Mcp-Session-Id')).toBe('s-1')
    expect(await reply.text()).toBe('data')

    host.handlers.notion_sign_in_refresh = () => true
    await expect(refreshSignIn()).resolves.toBe(HELD)
    host.handlers.notion_sign_in_refresh = () => false
    await expect(refreshSignIn()).resolves.toBeNull()
  })

  it('lets a Stop through while the host is still answering', async () => {
    host.handlers.notion_api_request = () => new Promise(() => undefined)
    const controller = new AbortController()
    const pending = apiFetch('https://api.notion.com/v1/search', {
      method: 'POST',
      headers: { authorization: `Bearer ${HELD}` },
      body: '{}',
      signal: controller.signal
    })
    controller.abort()
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
  })
})
