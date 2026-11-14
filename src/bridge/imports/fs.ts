/**
 * The `fs` the import run code uses: the calls upstream's
 * `*-import-run.ts` make on `node:fs`, each sent to Rust
 * (src-tauri/src/imports/files.rs), so the port's copies of those files can
 * stay line for line what upstream wrote.
 *
 * Paths are virtual: `/<kind>/<place>/<path inside it>`, where the place is
 * `picked` (the folder the dialog chose, read only), `source` (the folder a
 * running import reads), `staging` (its working folder under the app's data
 * folder) or `vault` (the vault it began in). Rust decides where each is and
 * what may happen there; the webview never names a real folder.
 *
 * A refusal keeps Node's error code (`EEXIST`, `ENOENT`) on `error.code`,
 * which upstream's code tests.
 */
import { invoke } from '../ipc'

export interface ImportDirent {
  name: string
  isDirectory(): boolean
  isFile(): boolean
  isSymbolicLink(): boolean
}

export interface ImportStats {
  isFile(): boolean
  isDirectory(): boolean
  size: number
  atimeMs: number
  mtimeMs: number
  birthtimeMs: number
  atime: Date
  mtime: Date
}

interface RawEntry {
  name: string
  kind: 'file' | 'dir' | 'symlink' | 'other'
}

interface RawStat {
  isFile: boolean
  isDirectory: boolean
  size: number
  atimeMs: number
  mtimeMs: number
  birthtimeMs: number
}

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    const failure = error as Error & { code?: string }
    const code = /^([A-Z][A-Z0-9_]+): /.exec(failure.message)?.[1]
    if (code) failure.code = code
    throw failure
  }
}

const ms = (time: Date | number): number => (typeof time === 'number' ? time : time.getTime())

export const constants = { COPYFILE_FICLONE: 2 }

type WriteOptions = 'utf8' | { encoding: 'utf8'; flag?: 'w' | 'wx' }

function readdir(dir: string): Promise<string[]>
function readdir(dir: string, options: { withFileTypes: true }): Promise<ImportDirent[]>
async function readdir(dir: string, options?: { withFileTypes: true }): Promise<string[] | ImportDirent[]> {
  const entries = await call<RawEntry[]>('import_fs_read_dir', { path: dir })
  if (!options?.withFileTypes) return entries.map((entry) => entry.name)
  return entries.map((entry) => ({
    name: entry.name,
    isDirectory: () => entry.kind === 'dir',
    isFile: () => entry.kind === 'file',
    isSymbolicLink: () => entry.kind === 'symlink'
  }))
}

async function stat(target: string): Promise<ImportStats> {
  const raw = await call<RawStat>('import_fs_stat', { path: target })
  return {
    isFile: () => raw.isFile,
    isDirectory: () => raw.isDirectory,
    size: raw.size,
    atimeMs: raw.atimeMs,
    mtimeMs: raw.mtimeMs,
    birthtimeMs: raw.birthtimeMs,
    atime: new Date(raw.atimeMs),
    mtime: new Date(raw.mtimeMs)
  }
}

export const fs = {
  constants,
  readdir,
  stat,
  readFile: (target: string, _encoding: 'utf8'): Promise<string> =>
    call('import_fs_read_text', { path: target }),
  writeFile: (target: string, text: string, options: WriteOptions): Promise<void> =>
    call('import_fs_write_text', {
      path: target,
      text,
      exclusive: typeof options === 'object' && options.flag === 'wx'
    }),
  appendFile: (target: string, text: string, _encoding: 'utf8'): Promise<void> =>
    call('import_fs_append_text', { path: target, text }),
  /** Plain copy; the mode is Node's clone hint, which the host's copy takes
   *  where the disk can. */
  copyFile: (from: string, to: string, _mode?: number): Promise<void> =>
    call('import_fs_copy', { from, to }),
  access: async (target: string): Promise<void> => {
    await stat(target)
  },
  utimes: (target: string, atime: Date | number, mtime: Date | number): Promise<void> =>
    call('import_fs_utimes', { path: target, atimeMs: ms(atime), mtimeMs: ms(mtime) }),
  /** Always recursive, as every call upstream makes is. */
  mkdir: (target: string, _options: { recursive: true }): Promise<void> =>
    call('import_fs_mkdir', { path: target }),
  /** Always `{ recursive: true, force: true }`, as every call upstream makes is. */
  rm: (target: string, _options: { recursive: true; force: true }): Promise<void> =>
    call('import_fs_rm', { path: target }),
  rename: (from: string, to: string): Promise<void> => call('import_fs_rename', { from, to })
}

/** Upstream's `moveIntoVault` (notion-import-run.ts), whole on the host: one
 *  rename, or a copy under a dot name renamed into place when the vault is
 *  on another volume. */
export function moveIntoVault(from: string, to: string): Promise<void> {
  return call('import_fs_move_into_vault', { from, to })
}

/** Upstream's `writeNoteCreatedAt` (note-creation-metadata.ts). */
export function writeNoteCreatedAt(root: string, rel: string, createdAt: number): Promise<void> {
  return call('import_write_note_created_at', { path: `${root}/${rel}`, createdAt })
}
