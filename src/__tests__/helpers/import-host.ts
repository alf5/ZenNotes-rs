/**
 * A Node stand-in for the Rust import host (src-tauri/src/imports), for the
 * tests of the port's import run code. It implements the file commands with
 * the same semantics as files.rs: virtual paths `/<kind>/<place>/…` resolve
 * against a real folder per place, errors carry Node's code at the start of
 * a plain string (as Rust's do), and a created date lands where sidecars.rs
 * puts it. What files.rs refuses is covered by its own Rust tests.
 */
import { promises as fs } from 'node:fs'
import path from 'node:path'

export type Places = Record<string, string>

export interface ImportHost {
  places: Places
  /** The real path a virtual one names. */
  resolve(virtual: string): string
  invoke(command: string, args: Record<string, unknown>): Promise<unknown>
  /** Commands seen, in order. */
  calls: Array<{ command: string; args: Record<string, unknown> }>
  /** Answers for the commands that aren't file operations. */
  handlers: Record<string, (args: Record<string, unknown>) => unknown>
}

const asString = (value: unknown): string => String(value)

export function createImportHost(places: Places): ImportHost {
  const resolve = (virtual: string): string => {
    const parts = virtual.split('/').filter(Boolean)
    const key = `/${parts[0]}/${parts[1]}`
    const base = places[key]
    if (!base) throw new Error(`EINVAL: not an import path, '${virtual}'`)
    return path.join(base, ...parts.slice(2))
  }

  const files: Record<string, (args: Record<string, unknown>) => Promise<unknown>> = {
    import_fs_read_dir: async ({ path: p }) =>
      (await fs.readdir(resolve(asString(p)), { withFileTypes: true })).map((entry) => ({
        name: entry.name,
        kind: entry.isSymbolicLink() ? 'symlink' : entry.isDirectory() ? 'dir' : entry.isFile() ? 'file' : 'other'
      })),
    import_fs_read_text: async ({ path: p }) => fs.readFile(resolve(asString(p)), 'utf8'),
    import_fs_stat: async ({ path: p }) => {
      const stat = await fs.stat(resolve(asString(p)))
      return {
        isFile: stat.isFile(),
        isDirectory: stat.isDirectory(),
        size: stat.size,
        atimeMs: stat.atimeMs,
        mtimeMs: stat.mtimeMs,
        birthtimeMs: stat.birthtimeMs
      }
    },
    import_fs_write_text: async ({ path: p, text, exclusive }) =>
      fs.writeFile(resolve(asString(p)), asString(text), { encoding: 'utf8', flag: exclusive ? 'wx' : 'w' }),
    import_fs_append_text: async ({ path: p, text }) => fs.appendFile(resolve(asString(p)), asString(text), 'utf8'),
    import_fs_mkdir: async ({ path: p }) => {
      await fs.mkdir(resolve(asString(p)), { recursive: true })
    },
    import_fs_copy: async ({ from, to }) => fs.copyFile(resolve(asString(from)), resolve(asString(to))),
    import_fs_utimes: async ({ path: p, atimeMs, mtimeMs }) =>
      fs.utimes(resolve(asString(p)), Number(atimeMs) / 1000, Number(mtimeMs) / 1000),
    import_fs_rm: async ({ path: p }) => fs.rm(resolve(asString(p)), { recursive: true, force: true }),
    import_fs_rename: async ({ from, to }) => fs.rename(resolve(asString(from)), resolve(asString(to))),
    import_fs_move_into_vault: async ({ from, to }) => {
      const target = resolve(asString(to))
      const taken = await fs.lstat(target).then(() => true, () => false)
      if (taken) throw new Error(`EEXIST: file already exists, rename '${asString(to)}'`)
      await fs.rename(resolve(asString(from)), target)
    },
    import_write_note_created_at: async ({ path: p, createdAt }) => {
      const parts = asString(p).split('/').filter(Boolean)
      const root = places[`/${parts[0]}/${parts[1]}`]
      const rel = parts.slice(2).join('/')
      if (!/\.(md|excalidraw)$/i.test(rel)) return
      const sidecar = path.join(root, '.zennotes', 'note-metadata', `${rel}.metadata.json`)
      await fs.mkdir(path.dirname(sidecar), { recursive: true })
      await fs.writeFile(sidecar, JSON.stringify({ version: 1, createdAt: Math.trunc(Number(createdAt)) }) + '\n')
    }
  }

  const host: ImportHost = {
    places,
    resolve,
    calls: [],
    handlers: {},
    invoke: async (command, args) => {
      host.calls.push({ command, args })
      const handler = host.handlers[command]
      if (handler) return handler(args)
      const file = files[command]
      if (!file) throw `no stand-in for ${command}`
      try {
        return await file(args)
      } catch (error) {
        // Rust rejects with a bare string, Node's code first.
        throw error instanceof Error ? error.message : error
      }
    }
  }
  return host
}

/** Upstream's `readNoteCreatedAt`, on the sidecar the host wrote. */
export async function readNoteCreatedAt(root: string, rel: string, fallback: number): Promise<number> {
  try {
    const raw = await fs.readFile(path.join(root, '.zennotes', 'note-metadata', `${rel}.metadata.json`), 'utf8')
    const value = JSON.parse(raw) as { createdAt?: unknown }
    return typeof value.createdAt === 'number' ? value.createdAt : fallback
  } catch {
    return fallback
  }
}
