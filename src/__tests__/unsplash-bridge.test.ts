/**
 * The Tauri host's Unsplash wiring: upstream's vendored client over Rust's
 * request and keychain commands. What matters here is the key, which only ever
 * crosses towards Rust, once, when it is pasted.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'

const calls: Array<{ command: string; args: Record<string, unknown> }> = []
let storedKey: string | null = null
let canStore = true
let nextReply: Record<string, unknown> = {}

vi.mock('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    calls.push({ command, args })
    switch (command) {
      case 'unsplash_key_can_store':
        return canStore
      case 'unsplash_key_is_set':
        return storedKey !== null
      case 'unsplash_key_set':
        storedKey = args.key as string
        return true
      case 'unsplash_key_clear':
        storedKey = null
        return undefined
      case 'unsplash_request':
        if (nextReply.offline) throw new Error('unreachable')
        return nextReply
      default:
        throw new Error(`unexpected ${command}`)
    }
  }
}))

const PASTED = 'A'.repeat(43)
const ok = (body: unknown = { total_pages: 0, results: [] }) => ({
  status: 200,
  ratelimitLimit: '50',
  ratelimitRemaining: '49',
  body: JSON.stringify(body)
})

async function fresh() {
  vi.resetModules()
  return (await import('../bridge/unsplash')).unsplash
}

const requests = () => calls.filter((c) => c.command === 'unsplash_request').map((c) => c.args)

describe('the Tauri Unsplash bridge', () => {
  beforeEach(() => {
    calls.length = 0
    storedKey = null
    canStore = true
    nextReply = ok()
  })

  it('reports whether a key is set, never the key', async () => {
    const unsplash = await fresh()
    expect(await unsplash.status()).toEqual({ set: false, store: 'system', canStore: true })
    storedKey = PASTED
    const again = await fresh()
    const status = await again.status()
    expect(status).toEqual({ set: true, store: 'system', canStore: true })
    expect(JSON.stringify(status)).not.toContain(PASTED)
  })

  it('checks a pasted key, keeps it, then asks with the stored one', async () => {
    const unsplash = await fresh()
    expect(await unsplash.save(`  ${PASTED}\n`)).toEqual({
      saved: true,
      check: { ok: true, remaining: 49, limit: 50 }
    })
    expect(storedKey).toBe(PASTED)
    expect(requests()[0]).toMatchObject({ key: PASTED })

    await unsplash.search('mountain lake', 1)
    const search = requests()[1]!
    expect(search.key).toBeNull()
    expect(String(search.url)).toContain('/search/photos?query=mountain+lake')
  })

  it('does not keep a key Unsplash refuses', async () => {
    const unsplash = await fresh()
    nextReply = { status: 401, ratelimitLimit: null, ratelimitRemaining: null, body: null }
    expect(await unsplash.save(PASTED)).toEqual({ saved: false, reason: 'invalid-key' })
    expect(storedKey).toBeNull()
  })

  it('refuses to save where the host has nowhere safe to keep a key', async () => {
    canStore = false
    const unsplash = await fresh()
    expect(await unsplash.save(PASTED)).toEqual({ saved: false, reason: 'cannot-store' })
    expect(requests()).toEqual([])
  })

  it('answers no-key without a request, and offline when the host cannot reach Unsplash', async () => {
    const unsplash = await fresh()
    expect(await unsplash.search('lake', 1)).toMatchObject({ ok: false, reason: 'no-key' })
    expect(requests()).toEqual([])

    storedKey = PASTED
    nextReply = { offline: true }
    const again = await fresh()
    expect(await again.search('lake', 1)).toMatchObject({ ok: false, reason: 'offline' })
  })

  it('reads the rate limit headers the host passes on', async () => {
    storedKey = PASTED
    nextReply = { status: 403, ratelimitLimit: '50', ratelimitRemaining: '0', body: null }
    const unsplash = await fresh()
    expect(await unsplash.search('lake', 1)).toMatchObject({
      ok: false,
      reason: 'rate-limited',
      limit: 50
    })
  })

  it('pings a checked download location only', async () => {
    storedKey = PASTED
    const unsplash = await fresh()
    await unsplash.trackDownload('https://evil.test/photos/a/download')
    expect(requests()).toEqual([])
    await unsplash.trackDownload('https://api.unsplash.com/photos/abc/download?ixid=1')
    expect(requests()[0]).toMatchObject({
      url: 'https://api.unsplash.com/photos/abc/download?ixid=1',
      key: null
    })
  })

  it('clears the key', async () => {
    storedKey = PASTED
    const unsplash = await fresh()
    await unsplash.clear()
    expect(storedKey).toBeNull()
    expect((await unsplash.status()).set).toBe(false)
  })
})
