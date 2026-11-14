/**
 * The Obsidian import as the app runs it: the port's copy of
 * apps/desktop/src/main/obsidian-import.ts. The folder is chosen through the
 * system's own dialog and kept by Rust: this side can ask about "the vault
 * that was picked" (`/obsidian/picked`, `/obsidian/source`) but can't name a
 * folder to read. The module state upstream keeps here (source, controller,
 * lastProgress) is shared through Rust; see host.ts.
 */
import { NotionAbortError } from '@shared/notion-import/client'
import type {
  NotionImportProgress,
  ObsidianImportScan
} from '@zennotes/bridge-contract/notion-import'
import { invoke } from '../ipc'
import * as host from './host'
import { runObsidianImport, scanObsidianVault } from './obsidian-import-run'

const KIND = 'obsidian'
let controller: AbortController | null = null
const broadcast = host.createBroadcast(KIND, 100)

export const obsidianImport = {
  status(): Promise<{ running: boolean; progress: NotionImportProgress | null }> {
    return host.status(KIND)
  },

  /** Null when the person cancels the dialog. */
  async pick(): Promise<ObsidianImportScan | null> {
    const picked = await invoke<string | null>('import_pick', { kind: KIND })
    if (!picked) return null
    await invoke('import_adopt', { kind: KIND })
    const scan = await scanObsidianVault(`/${KIND}/picked`)
    return { ...scan, path: picked, name: host.folderName(picked) }
  },

  async start(options: { includeTasks?: unknown; excluded?: unknown }): Promise<void> {
    if (controller) throw new Error('An import is already running.')
    const info = await host.begin(KIND)
    const running = new AbortController()
    controller = running
    const unwatch = host.abortOnStop(KIND, running)
    const excluded = Array.isArray(options.excluded)
      ? options.excluded.filter((id): id is string => typeof id === 'string')
      : []
    void runObsidianImport({
      source: info.source,
      stagingDir: info.stagingDir,
      root: info.root,
      notesRoot: info.notesRoot,
      existingNotes: info.existingNotes,
      existingAssets: info.existingAssets,
      excluded,
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
