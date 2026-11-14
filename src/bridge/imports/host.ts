/**
 * What the three import controllers (obsidian-import.ts, logseq-import.ts,
 * notion-import.ts) share on this host.
 *
 * Upstream runs an import in its main process, which every window reaches.
 * Here it runs in the page that started it, and Rust (src-tauri/src/imports)
 * keeps what upstream's module state kept: the picked folder, whether a run
 * is going and its last progress. So every window sees the same status and
 * progress, a Stop from any window reaches the run, and the progress of a
 * finished import is still there after a reload. A reload or a closed window
 * does end a run, as a Stop would (a Notion import can then continue).
 */
import { listen } from '@tauri-apps/api/event'
import type { NotionImportProgress } from '@zennotes/bridge-contract/notion-import'
import { invoke } from '../ipc'

export type ImportKind = 'obsidian' | 'logseq' | 'notion'

function newPageId(): string {
  try {
    return crypto.randomUUID()
  } catch {
    return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  }
}

/** This page. A run belongs to the page that started it. */
export const PAGE = newPageId()

/** Subscribe to a Tauri event, returning a synchronous unsubscribe. */
export function subscribe<T>(event: string, cb: (payload: T) => void): () => void {
  let unlisten = (): void => {}
  let cancelled = false
  void listen<T>(event, (e) => cb(e.payload)).then((fn) => {
    if (cancelled) fn()
    else unlisten = fn
  })
  return () => {
    cancelled = true
    unlisten()
  }
}

/** Upstream's `*_IMPORT_ON_PROGRESS`, sent by Rust to every window. */
export function onProgress(kind: ImportKind, cb: (progress: NotionImportProgress) => void): () => void {
  return subscribe<NotionImportProgress>(`import://${kind}-progress`, cb)
}

export interface RunStatus {
  running: boolean
  progress: NotionImportProgress | null
}

export async function status(kind: ImportKind): Promise<RunStatus> {
  const { running, progress } = await invoke<RunStatus & { stopping: boolean }>('import_status', { kind, page: PAGE })
  return { running, progress }
}

/** What a run starts from, as virtual paths (fs.ts). */
export interface BeginInfo {
  root: string
  source: string
  stagingDir: string
  notesRoot: string
  existingNotes: string[]
  existingAssets: string[]
}

/** The checks at the top of upstream's start handler (a run already going,
 *  a remote workspace, no folder picked, a folder that holds the vault),
 *  then the run takes the open vault for its whole life. */
export function begin(kind: ImportKind): Promise<BeginInfo> {
  return invoke<BeginInfo>('import_begin', { kind, page: PAGE })
}

/** A run that began but couldn't start. */
export function end(kind: ImportKind): Promise<void> {
  return invoke<void>('import_end', { kind, page: PAGE })
}

export function stop(kind: ImportKind): Promise<void> {
  return invoke<void>('import_stop', { kind, page: PAGE })
}

export function dismiss(kind: ImportKind): Promise<void> {
  return invoke<void>('import_dismiss', { kind })
}

/**
 * Upstream's `broadcast`: progress settles at once, and otherwise goes out at
 * most every `spacingMs` (a big import reports per file). Reports go in
 * order, so a late one never lands after the end.
 */
export function createBroadcast(
  kind: ImportKind,
  spacingMs: number
): (progress: NotionImportProgress) => Promise<void> {
  let lastSent = 0
  let chain: Promise<void> = Promise.resolve()
  return (progress) => {
    const settled = progress.phase === 'done' || progress.phase === 'stopped' || progress.phase === 'failed'
    const now = Date.now()
    if (!settled && now - lastSent < spacingMs) return chain
    lastSent = now
    chain = chain
      .then(() => invoke<void>('import_report', { kind, page: PAGE, progress }))
      .catch(() => undefined)
    return chain
  }
}

/** A Stop pressed in any window aborts the run this page holds, including
 *  one pressed before this page was listening. */
export function abortOnStop(kind: ImportKind, controller: AbortController): () => void {
  let cancelled = false
  let unlisten = (): void => {}
  void listen<string>('import://stop', (event) => {
    if (event.payload === kind) controller.abort()
  }).then(async (fn) => {
    if (cancelled) return fn()
    unlisten = fn
    const now = await invoke<{ stopping: boolean }>('import_status', { kind, page: PAGE }).catch(() => null)
    if (now?.stopping) controller.abort()
  })
  return () => {
    cancelled = true
    unlisten()
  }
}

/** The last part of a real path, on any system. */
export function folderName(folder: string): string {
  const trimmed = folder.replace(/[\\/]+$/, '')
  return trimmed.slice(Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\')) + 1) || trimmed
}

export const today = (): string => new Date().toISOString().slice(0, 10)
