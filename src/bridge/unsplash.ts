/**
 * The cover picker's Unsplash client, on the Tauri host. Upstream builds
 * `createUnsplashClient` (vendored shared-domain) in its main process with
 * net.fetch and the secret store; here it runs in the renderer, so its rules
 * and checks stay vendored, while Rust (src-tauri/src/unsplash.rs) keeps the
 * two things that must stay out of a renderer: the Access Key, in the OS
 * keychain, and the request, which it attaches the key to.
 *
 * The client only ever holds STORED_KEY, a stand-in that means "the one Rust
 * keeps". A pasted key crosses once, to be checked and saved, and the client
 * is rebuilt straight after so the real one is not left in memory. The
 * per-minute cap is the client's, so it counts per window here.
 */
import { invoke } from '@tauri-apps/api/core'
import {
  createUnsplashClient,
  type UnsplashClient,
  type UnsplashFetch
} from '@zennotes/shared-domain/unsplash'

/** Shaped like a key, or the client would refuse it. */
const STORED_KEY = 'key-held-by-the-rust-host'

interface UnsplashReply {
  status: number
  ratelimitLimit: string | null
  ratelimitRemaining: string | null
  body: string | null
}

const fetchThroughHost: UnsplashFetch = async (url, init) => {
  const authorization = (init.headers as Record<string, string>).authorization ?? ''
  const key = authorization.replace(/^Client-ID /, '')
  const reply = await invoke<UnsplashReply>('unsplash_request', {
    url,
    key: key === STORED_KEY ? null : key
  })
  return {
    ok: reply.status >= 200 && reply.status < 300,
    status: reply.status,
    headers: {
      get: (name: string) => {
        const header = name.toLowerCase()
        if (header === 'x-ratelimit-limit') return reply.ratelimitLimit
        if (header === 'x-ratelimit-remaining') return reply.ratelimitRemaining
        return null
      }
    },
    json: async () => JSON.parse(reply.body ?? '')
  }
}

let client: Promise<UnsplashClient> | null = null

function current(): Promise<UnsplashClient> {
  client ??= invoke<boolean>('unsplash_key_can_store')
    .catch(() => false)
    .then((canStore) =>
      createUnsplashClient({
        fetch: fetchThroughHost,
        store: 'system',
        keys: {
          get: async () => ((await invoke<boolean>('unsplash_key_is_set')) ? STORED_KEY : null),
          set: (key) => invoke<boolean>('unsplash_key_set', { key }),
          delete: () => invoke<void>('unsplash_key_clear'),
          canStore: () => canStore
        }
      })
    )
  return client
}

export const unsplash: UnsplashClient = {
  status: async () => (await current()).status(),
  save: async (key) => {
    const result = await (await current()).save(key)
    client = null
    return result
  },
  test: async () => (await current()).test(),
  clear: async () => {
    await (await current()).clear()
    client = null
  },
  search: async (query, page) => (await current()).search(query, page),
  trackDownload: async (location) => (await current()).trackDownload(location)
}
