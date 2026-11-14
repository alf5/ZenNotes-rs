/**
 * One Notion import, from reading the workspace to the finished folder in the
 * vault. Everything is built in a staging folder outside the vault and moved in
 * as one step, so the vault never holds half an import, a sync never uploads
 * one, and undoing an import means deleting one folder.
 *
 * No electron in here: the run takes its fetch and its folders as arguments,
 * so a test can drive all of it against a stand-in for Notion.
 *
 * ZenNotes-rs: the port's copy of apps/desktop/src/main/notion-import-run.ts,
 * changed only where it touches the disk or the network: `fs` and `path` are
 * ./fs and ./path, whose paths are virtual and whose every call Rust checks
 * (src-tauri/src/imports); a file is downloaded by the host straight into
 * staging (`FileFetch` below), since the webview can neither reach it nor
 * stream it to disk; `hashKey` uses WebCrypto, so it is async; and
 * `moveIntoVault` is one host call. Re-diff it against upstream on each
 * re-vendor.
 */
import { fs, moveIntoVault as moveIntoVaultOnHost } from './fs'
import path from './path'
import { assembleNotionImport, type NotionFileTimes } from '@shared/notion-import/assemble'
import { NotionAbortError, type NotionClient } from '@shared/notion-import/client'
import {
  collectNotionWorkspace,
  type NotionImportStore,
  type NotionStoredFile
} from '@shared/notion-import/collect'
import { NOTION_REPORT_NAME } from '@shared/notion-import/plan-paths'
import type {
  NotionImportProgress,
  NotionImportResult
} from '@zennotes/bridge-contract/notion-import'
import { writeNoteCreatedAt } from './fs'

export const NOTION_IMPORT_FOLDER = 'Notion Import'

const hashKey = async (key: string): Promise<string> => {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(key))
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('')
}

const EXT_BY_TYPE: Record<string, string> = {
  'image/png': '.png',
  'image/jpeg': '.jpg',
  'image/gif': '.gif',
  'image/webp': '.webp',
  'image/svg+xml': '.svg',
  'image/avif': '.avif',
  'application/pdf': '.pdf',
  'audio/mpeg': '.mp3',
  'audio/mp4': '.m4a',
  'video/mp4': '.mp4',
  'video/webm': '.webm'
}

/** A page can point at any URL, and this fetch runs on the person's machine,
 *  inside their network. Nothing on it gets fetched because a note said so. */
export function isPrivateHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, '')
  return (
    host === 'localhost' ||
    host.endsWith('.localhost') ||
    host.endsWith('.local') ||
    host.endsWith('.internal') ||
    host === '::1' ||
    host.startsWith('fc') ||
    host.startsWith('fd') ||
    host.startsWith('fe80') ||
    /^(127|10|0)\./.test(host) ||
    /^192\.168\./.test(host) ||
    /^169\.254\./.test(host) ||
    /^172\.(1[6-9]|2\d|3[01])\./.test(host)
  )
}

/** Names the type where the URL gave no extension: an outside image is often
 *  `/photo-123?w=800`, and with no extension it wouldn't embed. */
function nameWithExtension(name: string, contentType: string | null): string {
  if (/\.[a-z0-9]{1,8}$/i.test(name)) return name
  const ext = contentType ? EXT_BY_TYPE[contentType.split(';')[0].trim().toLowerCase()] : undefined
  return ext ? `${name}${ext}` : name
}

/** Fetches `url` and, unless it answered with an error or (when
 *  `refuseHtml`) a web page, writes its body to `target` in staging. On the
 *  app this is the host's download (notion-import.ts). */
export type FileFetch = (
  url: string,
  target: string,
  init: { signal?: AbortSignal; refuseHtml: boolean }
) => Promise<{ status: number; contentType: string | null; saved: boolean }>

/** Reads and writes in staging. Records and files are named by a hash of their
 *  key: keys hold ids and property names, which aren't filename material. */
export function createStagingStore(
  dir: string,
  fetchFile: FileFetch,
  options: { signal?: AbortSignal; allowInsecureFiles?: boolean } = {}
): NotionImportStore {
  const recordPath = async (key: string): Promise<string> => path.join(dir, 'records', `${await hashKey(key)}.json`)
  const filePath = async (key: string): Promise<string> => path.join(dir, 'files', await hashKey(key))
  const fileIndexPath = async (key: string): Promise<string> => path.join(dir, 'files', `${await hashKey(key)}.json`)

  const writeAtomic = async (target: string, content: string): Promise<void> => {
    await fs.mkdir(path.dirname(target), { recursive: true })
    const temporary = `${target}.tmp`
    await fs.writeFile(temporary, content, 'utf8')
    await fs.rename(temporary, target)
  }

  return {
    readRecord: async <T,>(key: string): Promise<T | null> => {
      try {
        return JSON.parse(await fs.readFile(await recordPath(key), 'utf8')) as T
      } catch {
        return null
      }
    },
    writeRecord: async (key, value) => writeAtomic(await recordPath(key), JSON.stringify(value)),
    hasFile: async (key) => {
      try {
        await fs.access(await fileIndexPath(key))
        return true
      } catch {
        return false
      }
    },
    downloadFile: async (key, url, suggestedName) => {
      const { protocol, hostname } = new URL(url)
      if (protocol !== 'https:' && !(options.allowInsecureFiles && protocol === 'http:')) {
        throw new Error(`Refused a file link over ${protocol}`)
      }
      if (!options.allowInsecureFiles && isPrivateHost(hostname)) {
        throw new Error('Refused a file link to a private address')
      }
      const target = await filePath(key)
      await fs.mkdir(path.dirname(target), { recursive: true })
      const temporary = `${target}.tmp`
      // Streamed by the host: an attachment can be far bigger than this
      // process wants to hold. A link that answers with a web page isn't the
      // file it named, so the host keeps none.
      const response = await fetchFile(url, temporary, {
        signal: options.signal,
        refuseHtml: !/\.html?$/i.test(suggestedName)
      })
      if (response.status < 200 || response.status >= 300) {
        throw new Error(`The file link answered ${response.status}`)
      }
      if (!response.saved) throw new Error('The file link answered with a web page')
      const contentType = response.contentType
      await fs.rename(temporary, target)
      await writeAtomic(
        await fileIndexPath(key),
        JSON.stringify({ key, name: nameWithExtension(suggestedName, contentType) })
      )
    },
    listFiles: async () => {
      const filesDir = path.join(dir, 'files')
      const names = await fs.readdir(filesDir).catch(() => [] as string[])
      const out: NotionStoredFile[] = []
      for (const name of names.filter((entry) => entry.endsWith('.json')).sort()) {
        try {
          out.push(JSON.parse(await fs.readFile(path.join(filesDir, name), 'utf8')) as NotionStoredFile)
        } catch {
          // A torn index means the file never finished: it was never placed.
        }
      }
      return out.sort((a, b) => a.key.localeCompare(b.key))
    }
  }
}

function insideOut(outDir: string, relative: string): string {
  const target = path.resolve(outDir, relative)
  if (!target.startsWith(path.resolve(outDir) + path.sep)) {
    throw new Error(`Import path escapes its folder: ${relative}`)
  }
  return target
}

async function pathExists(target: string): Promise<boolean> {
  try {
    await fs.access(target)
    return true
  } catch {
    return false
  }
}

export async function unusedFolderName(parent: string, base: string): Promise<string> {
  let name = base
  let n = 1
  while (await pathExists(path.join(parent, name))) {
    n += 1
    name = `${base} ${n}`
  }
  return name
}

export async function moveIntoVault(from: string, to: string): Promise<void> {
  // One rename; when the vault is on another volume, a copy under a dot name
  // (which no walker descends into) that appears whole with a final rename.
  // Both on the host: src-tauri/src/imports/files.rs.
  await moveIntoVaultOnHost(from, to)
}

export interface NotionRunOptions {
  client: NotionClient
  store: NotionImportStore
  stagingDir: string
  /** The vault. */
  root: string
  /** Where the vault keeps its notes, with system-folder remaps applied. */
  notesRoot: string
  includeTasks: boolean
  excluded?: string[]
  onProgress: (progress: NotionImportProgress) => void
  signal?: AbortSignal
  today?: string
}

export async function runNotionImport(options: NotionRunOptions): Promise<NotionImportResult> {
  const { client, store, stagingDir, root, notesRoot, onProgress } = options

  // The client only stops at its next request, and neither writing nor the
  // move makes one: a Stop pressed after the last page must still hold.
  const stopIfAsked = (): void => {
    if (options.signal?.aborted) throw new NotionAbortError()
  }

  const collected = await collectNotionWorkspace({ client, store, excluded: options.excluded, onProgress })
  stopIfAsked()

  await fs.mkdir(notesRoot, { recursive: true })
  const folderName = await unusedFolderName(notesRoot, NOTION_IMPORT_FOLDER)
  const finalDir = path.join(notesRoot, folderName)
  const vaultFolder = path.relative(root, finalDir).split(path.sep).join('/')
  const outDir = path.join(stagingDir, 'out')
  await fs.rm(outDir, { recursive: true, force: true })
  await fs.mkdir(outDir, { recursive: true })

  const createdDates = new Map<string, number>()
  const summary = await assembleNotionImport({
    collected,
    store,
    sink: {
      writeText: async (relative: string, content: string, times?: NotionFileTimes) => {
        const target = insideOut(outDir, relative)
        await fs.mkdir(path.dirname(target), { recursive: true })
        await fs.writeFile(target, content, 'utf8')
        if (times?.updatedMs) {
          const when = new Date(times.updatedMs)
          await fs.utimes(target, when, when)
        }
        if (times?.createdMs) createdDates.set(relative, times.createdMs)
      },
      placeFile: async (key: string, relative: string) => {
        const target = insideOut(outDir, relative)
        await fs.mkdir(path.dirname(target), { recursive: true })
        await fs.copyFile(path.join(stagingDir, 'files', await hashKey(key)), target, fs.constants.COPYFILE_FICLONE)
      }
    },
    import: { vaultFolder, includeTasks: options.includeTasks, today: options.today },
    onProgress: (done, total) => {
      stopIfAsked()
      onProgress({ phase: 'writing', done, total })
    }
  })

  stopIfAsked()
  await moveIntoVault(outDir, finalDir)
  // The move kept modified times. Created dates live beside the notes, the
  // same place a rename keeps them.
  for (const [relative, createdAt] of createdDates) {
    await writeNoteCreatedAt(root, `${vaultFolder}/${relative}`, createdAt).catch(() => undefined)
  }
  await fs.rm(stagingDir, { recursive: true, force: true })

  return {
    folder: vaultFolder,
    reportPath: `${vaultFolder}/${NOTION_REPORT_NAME}.md`,
    pages: summary.pages,
    databases: summary.databases,
    rows: summary.rows,
    files: summary.files,
    issues: summary.issues.length
  }
}
