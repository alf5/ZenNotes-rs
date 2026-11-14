/**
 * The Logseq import as the app runs it: the port's copy of
 * apps/desktop/src/main/logseq-import.ts. As with Obsidian, the graph folder
 * is chosen through the system's own dialog and kept by Rust: this side can
 * ask about "the graph that was picked" but can't name a folder to read.
 */
import { NotionAbortError } from '@shared/notion-import/client'
import type {
  LogseqDailyNotePlace,
  LogseqImportScan,
  NotionImportProgress
} from '@zennotes/bridge-contract/notion-import'
import { invoke } from '../ipc'
import * as host from './host'
import { LOGSEQ_IMPORT_FOLDER, runLogseqImport, scanLogseqGraph } from './logseq-import-run'

const KIND = 'logseq'
let controller: AbortController | null = null
const broadcast = host.createBroadcast(KIND, 100)

/** The renderer's daily-note places, as plain data; the run checks each path again. */
function readDailyNotes(value: unknown): Record<string, LogseqDailyNotePlace> | undefined {
  if (!value || typeof value !== 'object') return undefined
  const out: Record<string, LogseqDailyNotePlace> = {}
  for (const [date, place] of Object.entries(value as Record<string, unknown>)) {
    if (!place || typeof place !== 'object') continue
    const { subpath, title, exists } = place as Record<string, unknown>
    if (typeof subpath !== 'string' || typeof title !== 'string') continue
    out[date] = { subpath, title, exists: exists === true }
  }
  return out
}

export const logseqImport = {
  status(): Promise<{ running: boolean; progress: NotionImportProgress | null }> {
    return host.status(KIND)
  },

  /** Null when the person cancels the dialog. */
  async pick(): Promise<LogseqImportScan | null> {
    const picked = await invoke<string | null>('import_pick', { kind: KIND })
    if (!picked) return null
    const scan = await scanLogseqGraph(`/${KIND}/picked`)
    // Only a folder that scanned becomes the graph (`source = picked`).
    await invoke('import_adopt', { kind: KIND })
    return { ...scan, path: picked, name: host.folderName(picked) }
  },

  async start(options: { folder?: unknown; dailyNotes?: unknown; bullets?: unknown; includeTasks?: unknown }): Promise<void> {
    if (controller) throw new Error('An import is already running.')
    const info = await host.begin(KIND)
    const running = new AbortController()
    controller = running
    const unwatch = host.abortOnStop(KIND, running)
    void runLogseqImport({
      source: info.source,
      stagingDir: info.stagingDir,
      root: info.root,
      notesRoot: info.notesRoot,
      existingNotes: info.existingNotes,
      folder: typeof options.folder === 'string' && options.folder.trim() ? options.folder : LOGSEQ_IMPORT_FOLDER,
      dailyNotes: readDailyNotes(options.dailyNotes),
      bullets: options.bullets === true,
      includeTasks: options.includeTasks !== false,
      today: host.today(),
      onProgress: (progress) => void broadcast(progress),
      signal: running.signal
    })
      // A finished import forgets its folder (`source = null`) on the host.
      .then((result) => broadcast({ phase: 'done', done: 1, total: 1, result }))
      .catch((error: unknown) => {
        const stopped = error instanceof NotionAbortError || running.signal.aborted
        return broadcast(
          stopped
            ? { phase: 'stopped', done: 0, total: 0 }
            : { phase: 'failed', done: 0, total: 0, error: error instanceof Error ? error.message : String(error) }
        )
      })
      .finally(() => {
        controller = null
        unwatch()
      })
  },

  stop(): Promise<void> {
    return host.stop(KIND)
  },

  /** A finished result has been seen. */
  dismiss(): Promise<void> {
    return host.dismiss(KIND)
  }
}
