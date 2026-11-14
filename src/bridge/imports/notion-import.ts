/**
 * The Notion import as the app runs it: the port's copy of
 * apps/desktop/src/main/notion-import.ts. The vendored clients, the tree and
 * the run (notion-import-run.ts) work here in the webview; every request
 * goes through Rust, which holds the credentials (the OS keychain) and sends
 * only to Notion (notion-host.ts). Signing in is Rust's whole
 * (src-tauri/src/imports/notion_auth.rs), so its tokens never come here.
 *
 * One run at a time. It keeps its vault for the whole run, so switching
 * vaults partway doesn't change where the import lands.
 *
 * Not here: upstream's refusal to sign in or keep a token without secure
 * storage (this port keeps them for the run instead, never in a file).
 */
import { getVersion } from '@tauri-apps/api/app'
import {
  NotionAbortError,
  NotionApiError,
  NOTION_API_BASE_URL,
  createNotionClient,
  type NotionClient
} from '@shared/notion-import/client'
import { createMcpNotionClient } from '@shared/notion-import/mcp-source'
import { NOTION_MCP_BASE_URL, createNotionMcpTransport } from '@shared/notion-import/mcp-transport'
import { notionTreeChildren, notionTreeRoots } from '@shared/notion-import/tree'
import type {
  NotionImportProgress,
  NotionImportStatus,
  NotionImportTokenResult,
  NotionImportTree,
  NotionTreeItem
} from '@zennotes/bridge-contract/notion-import'
import { invoke } from '../ipc'
import * as host from './host'
import { HELD, apiFetch, downloadThroughHost, mcpFetch, refreshSignIn } from './notion-host'
import { createStagingStore, runNotionImport } from './notion-import-run'

const KIND = 'notion'

function notionErrorMessage(error: unknown): string {
  if (error instanceof NotionApiError) {
    if (error.isAuth) return 'Notion refused this sign-in. Sign in again.'
    return error.message
  }
  return error instanceof Error ? error.message : String(error)
}

/** A run can take hours, and nobody watches a progress bar that long. The
 *  host shows it only while no window has focus. */
function notifyIfAway(title: string, body: string): void {
  void invoke('import_notify_if_away', { title, body }).catch(() => undefined)
}

let controller: AbortController | null = null
// A big workspace reports per page. The window needs a few frames a second.
const broadcast = host.createBroadcast(KIND, 150)

async function clientFor(signal?: AbortSignal): Promise<NotionClient> {
  const credentials = await invoke<{ signedIn: boolean; tokenSet: boolean }>('notion_credentials')
  if (credentials.signedIn) {
    return createMcpNotionClient(
      createNotionMcpTransport({
        fetch: mcpFetch,
        baseUrl: NOTION_MCP_BASE_URL,
        accessToken: async () => HELD,
        refreshToken: refreshSignIn,
        signal,
        clientVersion: await getVersion().catch(() => undefined)
      })
    )
  }
  if (!credentials.tokenSet) throw new Error('Sign in to Notion first.')
  return createNotionClient({ fetch: apiFetch, token: HELD, baseUrl: NOTION_API_BASE_URL, signal })
}

/** A client kept between picker calls: reading the workspace a level at a
 *  time is one conversation, and a new one per click would redo its setup. */
let pickerClient: NotionClient | null = null

export const notionImport = {
  status(): Promise<NotionImportStatus> {
    return invoke<NotionImportStatus>('notion_import_status', { page: host.PAGE })
  },

  async signIn(): Promise<void> {
    pickerClient = null
    await invoke('notion_sign_in')
  },

  cancelSignIn(): Promise<void> {
    return invoke('notion_sign_in_cancel')
  },

  async signOut(): Promise<void> {
    pickerClient = null
    await invoke('notion_sign_out')
  },

  async saveToken(token: unknown): Promise<NotionImportTokenResult> {
    const value = typeof token === 'string' ? token.trim() : ''
    if (!value) return { ok: false, error: 'Paste a token first.' }
    // Checked before it's kept, so a bad paste fails here and not an hour in.
    const client = createNotionClient({ fetch: apiFetch, token: value, baseUrl: NOTION_API_BASE_URL })
    try {
      await client.me()
    } catch (error) {
      if (error instanceof NotionApiError && error.isAuth) {
        return { ok: false, error: 'Notion refused this token. Check it and save it again.' }
      }
      return { ok: false, error: notionErrorMessage(error) }
    }
    pickerClient = null
    return (await invoke<boolean>('notion_token_set', { token: value }))
      ? { ok: true }
      : { ok: false, error: 'The token could not be saved.' }
  },

  async tree(): Promise<NotionImportTree> {
    try {
      pickerClient = await clientFor()
      const me = await pickerClient.me()
      return {
        workspace: me.bot?.workspace_name?.trim() || 'Notion',
        items: await notionTreeRoots(pickerClient)
      }
    } catch (error) {
      throw new Error(notionErrorMessage(error))
    }
  },

  async treeChildren(pageId: unknown): Promise<NotionTreeItem[]> {
    if (typeof pageId !== 'string' || !/^[0-9a-f-]{32,36}$/i.test(pageId)) throw new Error('Not a page.')
    try {
      pickerClient ??= await clientFor()
      return await notionTreeChildren(pickerClient, pageId)
    } catch (error) {
      throw new Error(notionErrorMessage(error))
    }
  },

  /** Starts in the background: progress arrives on onNotionImportProgress. */
  async start(options: { includeTasks?: unknown; excluded?: unknown }): Promise<void> {
    if (controller) throw new Error('An import is already running.')
    const info = await host.begin(KIND)
    const running = new AbortController()
    controller = running
    const unwatch = host.abortOnStop(KIND, running)
    try {
      const client = await clientFor(running.signal)
      const store = createStagingStore(info.stagingDir, downloadThroughHost, {
        signal: running.signal,
        allowInsecureFiles: await invoke<boolean>('notion_import_stand_in')
      })
      const excluded = Array.isArray(options.excluded)
        ? options.excluded.filter((id): id is string => typeof id === 'string')
        : []
      void runNotionImport({
        client,
        store,
        stagingDir: info.stagingDir,
        root: info.root,
        notesRoot: info.notesRoot,
        includeTasks: options.includeTasks === true,
        excluded,
        today: host.today(),
        onProgress: (progress: NotionImportProgress) => void broadcast(progress),
        signal: running.signal
      })
        .then((result) => {
          notifyIfAway('Notion import finished', `Your notes are in ${result.folder}.`)
          return broadcast({ phase: 'done', done: 1, total: 1, result })
        })
        .catch((error: unknown) => {
          const stopped = error instanceof NotionAbortError || running.signal.aborted
          if (!stopped) notifyIfAway('Notion import stopped', notionErrorMessage(error))
          return broadcast(
            stopped
              ? { phase: 'stopped', done: 0, total: 0 }
              : { phase: 'failed', done: 0, total: 0, error: notionErrorMessage(error) }
          )
        })
        .finally(() => {
          controller = null
          unwatch()
        })
    } catch (error) {
      controller = null
      unwatch()
      await host.end(KIND)
      throw new Error(notionErrorMessage(error))
    }
  },

  stop(): Promise<void> {
    return host.stop(KIND)
  },

  /** Throws away what a stopped run read, so the next one starts fresh. */
  discard(): Promise<void> {
    return invoke('notion_import_discard', { page: host.PAGE })
  }
}
