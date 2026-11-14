/**
 * The fetches the vendored Notion clients run on. Upstream gives them
 * net.fetch in its main process; here they run in the webview, whose fetch
 * would meet CORS and must never hold the credential, so each request goes
 * to Rust (src-tauri/src/imports/notion_http.rs), which sends it only to
 * Notion and attaches the token itself. The clients only ever hold HELD, a
 * stand-in that means "the one Rust keeps"; a pasted token crosses once, to
 * be checked before it's saved.
 */
import type { NotionFetch } from '@shared/notion-import/client'
import type { McpFetch } from '@shared/notion-import/mcp-transport'
import { invoke } from '../ipc'
import type { FileFetch } from './notion-import-run'

export const HELD = 'credential-held-by-the-rust-host'

interface HostReply {
  status: number
  headers: Record<string, string>
  body: string
}

/** Rejects as soon as the run is stopped, as an aborted fetch does; the
 *  host's request ends on its own. */
function abortable<T>(call: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return call
  if (signal.aborted) return Promise.reject(new DOMException('Aborted', 'AbortError'))
  return new Promise<T>((resolve, reject) => {
    const onAbort = (): void => reject(new DOMException('Aborted', 'AbortError'))
    signal.addEventListener('abort', onAbort, { once: true })
    call.then(
      (value) => {
        signal.removeEventListener('abort', onAbort)
        resolve(value)
      },
      (error: unknown) => {
        signal.removeEventListener('abort', onAbort)
        reject(error)
      }
    )
  })
}

function withoutAuthorization(headers: Record<string, string>): {
  headers: Record<string, string>
  bearer: string | null
} {
  const rest: Record<string, string> = {}
  let bearer: string | null = null
  for (const [name, value] of Object.entries(headers)) {
    if (name.toLowerCase() === 'authorization') bearer = value.replace(/^Bearer /, '')
    else rest[name] = value
  }
  return { headers: rest, bearer }
}

const readHeader = (reply: HostReply) => (name: string): string | null =>
  reply.headers[name.toLowerCase()] ?? null

/** The REST API, with the saved token (or a pasted one being checked). */
export const apiFetch: NotionFetch = async (url, init) => {
  const { headers, bearer } = withoutAuthorization(init.headers)
  const reply = await abortable(
    invoke<HostReply>('notion_api_request', {
      url,
      method: init.method,
      headers,
      body: init.body ?? null,
      token: bearer && bearer !== HELD ? bearer : null
    }),
    init.signal
  )
  return {
    status: reply.status,
    headers: { get: readHeader(reply) },
    json: async () => JSON.parse(reply.body) as unknown
  }
}

/** Notion's hosted MCP service, as the signed-in person. */
export const mcpFetch: McpFetch = async (url, init) => {
  const { headers } = withoutAuthorization(init.headers)
  const reply = await abortable(
    invoke<HostReply>('notion_mcp_request', { url, method: init.method, headers, body: init.body ?? null }),
    init.signal
  )
  return {
    status: reply.status,
    headers: { get: readHeader(reply) },
    text: async () => reply.body
  }
}

/** The MCP transport's `refreshToken`: HELD when Rust has a fresh token. */
export async function refreshSignIn(): Promise<string | null> {
  return (await invoke<boolean>('notion_sign_in_refresh')) ? HELD : null
}

/** The staging store's download, done by the host straight into staging. */
export const downloadThroughHost: FileFetch = (url, target, init) =>
  abortable(
    invoke<{ status: number; contentType: string | null; saved: boolean }>('import_notion_download', {
      url,
      target,
      refuseHtml: init.refuseHtml
    }),
    init.signal
  )
