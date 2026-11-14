/**
 * The port's clip links: upstream's contract (docs/web-clipper-link.md) under
 * `zennotes-rs://`, routed to the one workspace this app keeps open.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { beforeEach, describe, expect, it, vi } from 'vitest'

let clipboardCheck: Record<string, unknown> = { kind: 'notArrived' }
let workspaceInfo: Record<string, unknown> | null = { mode: 'local', profileId: null }
let currentVault: Record<string, unknown> | null = { root: '/home/me/Notes', name: 'Notes' }
const invoked: Array<{ command: string; args: Record<string, unknown> }> = []

vi.mock('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    invoked.push({ command, args })
    switch (command) {
      case 'clip_verified_clipboard_text':
        return clipboardCheck
      case 'clip_raise_window':
        return undefined
      case 'workspace_get_info':
        return workspaceInfo
      case 'vault_get_current':
        return currentVault
      case 'vault_list_local':
        return [
          { root: '/home/me/Notes', name: 'Notes', lastOpenedAt: 1 },
          { root: '/home/me/Work Vault', name: 'Acme docs', lastOpenedAt: 2 }
        ]
      case 'workspace_list_remote_profiles':
        return [{ id: 'p1', name: 'Home server', baseUrl: 'https://zen.example', hasCredential: true, vaultPath: null, lastConnectedAt: null }]
      default:
        throw new Error(`unexpected ${command}`)
    }
  }
}))

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ label: 'main' })
}))

vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }))

const { asUpstreamClipLink, decideClip, onClipNotice, onClipRequested, runClipLink } = await import(
  '../bridge/clip-links'
)
const { parseClipLink } = await import('@zennotes/shared-domain/clip-link')

/** A fixture link, or one built from parts because it is too long to write. */
type FixtureLink = string | { prefix: string; repeat: string; times: number; suffix: string }

describe('clip links under the port scheme', () => {
  const clips: unknown[] = []
  const notices: string[] = []
  onClipRequested((request) => clips.push(request))
  onClipNotice((notice) => notices.push(notice.message))

  beforeEach(() => {
    clips.length = 0
    notices.length = 0
    invoked.length = 0
    clipboardCheck = { kind: 'notArrived' }
    workspaceInfo = { mode: 'local', profileId: null }
    currentVault = { root: '/home/me/Notes', name: 'Notes' }
  })

  it('reads every link of the shared fixtures the same under the port scheme', () => {
    const fixtures = JSON.parse(
      readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'docs', 'web-clipper-link.fixtures.json'), 'utf8')
    ) as { accept: Array<{ link: FixtureLink }>; reject: Array<{ link: FixtureLink }>; notClip: string[] }
    const links = [...fixtures.accept, ...fixtures.reject].map((f) =>
      typeof f.link === 'string' ? f.link : f.link.prefix + f.link.repeat.repeat(f.link.times) + f.link.suffix
    )
    expect(links.length).toBeGreaterThan(30)
    for (const link of links) {
      expect(link).toMatch(/^zennotes:/i)
      const ported = link.replace(/^zennotes:/i, 'zennotes-rs:')
      expect(parseClipLink(asUpstreamClipLink(ported))).toEqual(parseClipLink(link))
    }
    for (const link of fixtures.notClip) {
      expect(parseClipLink(asUpstreamClipLink(link.replace(/^zennotes:/, 'zennotes-rs:')))).toBeNull()
    }
  })

  it('reads zennotes-rs links with the vendored parser', () => {
    const parsed = parseClipLink(asUpstreamClipLink('ZenNotes-RS://new?file=Clippings%2FArticle&content=hi'))
    expect(parsed).toMatchObject({ ok: true, request: { action: 'new', folder: 'Clippings', name: 'Article' } })
    expect(parseClipLink(asUpstreamClipLink('zennotes-rs://open?path=a.md'))).toBeNull()
  })

  it('hands a clip for the open vault to app-core', async () => {
    await runClipLink('zennotes-rs://new?file=Clippings%2FArticle&content=Body')
    expect(clips).toEqual([
      {
        vaultRoot: '/home/me/Notes',
        action: 'new',
        folder: 'Clippings',
        name: 'Article',
        text: 'Body',
        position: 'create',
        silent: false
      }
    ])
    expect(notices).toEqual([])
  })

  it('files the clipboard only when Rust vouches for it', async () => {
    const hash = 'a'.repeat(64)
    await runClipLink(`zennotes-rs://daily?clipboard=${hash}&silent`)
    expect(invoked.find((c) => c.command === 'clip_verified_clipboard_text')?.args).toEqual({ sha256: hash })
    expect(clips).toEqual([])
    expect(notices).toEqual(['The clip did not arrive (the clipboard changed). Clip it again.'])

    clipboardCheck = { kind: 'verified', text: 'From the page' }
    await runClipLink(`zennotes-rs://daily?clipboard=${hash}&silent`)
    expect(clips).toMatchObject([{ action: 'daily', text: 'From the page', silent: true }])
  })

  it('refuses a malformed link with upstream wording', async () => {
    await runClipLink('zennotes-rs://new?file=..%2Fescape&content=x')
    expect(notices).toEqual(['That clip names a place in the vault ZenNotes cannot write to.'])
  })

  it('matches a vault by name or folder name, and refuses one that is not open', async () => {
    await runClipLink('zennotes-rs://new?vault=notes&content=x')
    expect(clips).toHaveLength(1)
    await runClipLink('zennotes-rs://new?vault=Work%20Vault&content=x')
    expect(notices).toEqual(['That clip is for "Acme docs". Open that vault in ZenNotes-rs, then clip again.'])
    await runClipLink('zennotes-rs://new?vault=Nowhere&content=x')
    expect(notices[1]).toBe('No vault is named "Nowhere". Check the vault name in the web clipper.')
  })

  it('routes a server profile clip only while that server is open', () => {
    const request = { vault: 'Home server', action: 'new', folder: '', name: null, body: { content: 'x' }, position: 'create', silent: false } as never
    const choices = [{ kind: 'remote' as const, profileId: 'p1', name: 'Home server' }]
    expect(decideClip(request, 'x', { remote: false, localRoot: '/n', remoteProfileId: null }, choices)).toEqual({
      kind: 'refused',
      message: 'Connect to "Home server" in ZenNotes-rs, then clip again.'
    })
    expect(decideClip(request, 'x', { remote: true, localRoot: null, remoteProfileId: 'p1' }, choices)).toMatchObject({
      kind: 'apply',
      request: { vaultRoot: null, text: 'x' }
    })
  })

  it('asks for a vault when none is open', async () => {
    currentVault = null
    await runClipLink('zennotes-rs://new')
    expect(notices).toEqual(['Open a vault in ZenNotes-rs, then clip again.'])
  })
})
