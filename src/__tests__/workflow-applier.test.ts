/**
 * The workflow applier (src/bridge/workflows.ts) against a real temp vault.
 *
 * The applier is the port's own copy of upstream's workflow-apply.ts, so a
 * re-vendor never updates it; these cases pin the guarantees upstream's own
 * tests pin for v2.73 (#839 and the symlink work): a path op carries the
 * note's comments, creation date and agent edit record, undo puts back every
 * byte, a leftover discussion refuses the move, and a moved symlink stays a
 * link pointing at the same file.
 *
 * `invoke` is replaced by Node implementations of the Rust primitives the
 * applier calls (src-tauri/src/ipc/vault_cmds.rs), with the same semantics:
 * paths are vault-relative, writes are temp file plus rename, and the sidecar
 * paths are derived exactly as sidecars.rs derives them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

let root = ''

const abs = (rel: string): string => path.join(root, ...rel.split('/'))
const readOrNull = (rel: string): string | null => {
  try {
    return fs.readFileSync(abs(rel), 'utf8')
  } catch {
    return null
  }
}
const lexists = (p: string): boolean => {
  try {
    fs.lstatSync(p)
    return true
  } catch {
    return false
  }
}
const writeAtomic = (target: string, text: string): void => {
  fs.mkdirSync(path.dirname(target), { recursive: true })
  const tmp = path.join(path.dirname(target), `.${path.basename(target)}.tmp`)
  fs.writeFileSync(tmp, text)
  fs.renameSync(tmp, target)
}
const throughLinks = (p: string): string => {
  try {
    if (!fs.lstatSync(p).isSymbolicLink()) return p
  } catch {
    return p
  }
  try {
    return fs.realpathSync(p)
  } catch {
    return path.resolve(path.dirname(p), fs.readlinkSync(p))
  }
}
const pruneBelowTop = (start: string): void => {
  let dir = start
  while (dir.startsWith(root + path.sep) && path.dirname(dir) !== root) {
    try {
      fs.rmdirSync(dir)
    } catch {
      return
    }
    dir = path.dirname(dir)
  }
}
const SIDECAR_ROOTS = ['.zennotes/comments', '.zennotes/note-metadata', '.zennotes/agent-edits']

function rebaseMovedLink(from: string, to: string): void {
  if (!fs.lstatSync(to).isSymbolicLink()) return
  const text = fs.readlinkSync(to)
  if (path.isAbsolute(text) || path.dirname(from) === path.dirname(to)) return
  const next = path.relative(path.dirname(to), path.resolve(path.dirname(from), text))
  if (next === text) return
  const tmp = `${to}_relink_tmp`
  fs.symlinkSync(next, tmp)
  fs.renameSync(tmp, to)
}

const commands: Record<string, (args: Record<string, unknown>) => unknown> = {
  vault_get_settings: () => ({}),
  db_read_text: ({ relPath }) => readOrNull(relPath as string),
  db_write_text: ({ relPath, text }) => writeAtomic(abs(relPath as string), text as string),
  db_write_through_links: ({ relPath, text }) =>
    writeAtomic(throughLinks(abs(relPath as string)), text as string),
  db_remove_through_links: ({ relPath }) => {
    const target = throughLinks(abs(relPath as string))
    fs.rmSync(target, { force: true })
    if (target.startsWith(root)) pruneBelowTop(path.dirname(target))
  },
  db_delete_file: ({ relPath }) => fs.rmSync(abs(relPath as string), { force: true }),
  db_append_text: ({ relPath, text }) => {
    fs.mkdirSync(path.dirname(abs(relPath as string)), { recursive: true })
    fs.appendFileSync(abs(relPath as string), text as string)
  },
  db_exists: ({ relPath }) => fs.existsSync(abs(relPath as string)),
  db_list_dir: ({ relPath }) => {
    try {
      return fs
        .readdirSync(abs(relPath as string), { withFileTypes: true })
        .filter((e) => e.isFile())
        .map((e) => e.name)
        .sort()
    } catch {
      return []
    }
  },
  db_file_size: ({ relPath }) => {
    try {
      return fs.statSync(abs(relPath as string)).size
    } catch {
      return 0
    }
  },
  db_mkdir: ({ relPath }) => fs.mkdirSync(abs(relPath as string), { recursive: true }),
  db_path_taken: ({ relPath }) => lexists(abs(relPath as string)),
  db_link_text: ({ relPath }) => {
    try {
      const p = abs(relPath as string)
      return fs.lstatSync(p).isSymbolicLink() ? fs.readlinkSync(p) : null
    } catch {
      return null
    }
  },
  db_respell_link: ({ relPath, text }) => {
    const p = abs(relPath as string)
    const names = (t: string): string => path.resolve(path.dirname(p), t)
    if (names(fs.readlinkSync(p)) !== names(text as string)) throw new Error('Refusing to point a link somewhere new')
    const tmp = `${p}_relink_tmp`
    fs.symlinkSync(text as string, tmp)
    fs.renameSync(tmp, p)
  },
  db_rename_entry: ({ oldRel, newRel }) => {
    const from = abs(oldRel as string)
    const to = abs(newRel as string)
    fs.mkdirSync(path.dirname(to), { recursive: true })
    fs.renameSync(from, to)
    rebaseMovedLink(from, to)
  },
  db_remove_entry: ({ relPath }) => {
    fs.rmSync(abs(relPath as string), { force: true })
    pruneBelowTop(path.dirname(abs(relPath as string)))
  },
  db_sidecar_rel: ({ noteRel, kind }) => {
    if (kind === 'comments') return `.zennotes/comments/${noteRel}.comments.json`
    if (kind === 'metadata') return `.zennotes/note-metadata/${noteRel}.metadata.json`
    return `.zennotes/agent-edits/${noteRel}.json`
  },
  db_prepare_note_creation: ({ relPath }) => {
    const meta = abs(`.zennotes/note-metadata/${relPath}.metadata.json`)
    if (!fs.existsSync(abs(relPath as string)) || fs.existsSync(meta)) return
    writeAtomic(meta, '{"version":1,"createdAt":1000}\n')
  },
  db_prune_sidecar_dirs: ({ relDir }) => {
    const base = SIDECAR_ROOTS.map((r) => abs(r)).find((b) => abs(relDir as string).startsWith(b + path.sep))
    if (!base) return
    let dir = abs(relDir as string)
    while (dir !== base && dir.startsWith(base)) {
      try {
        if (fs.readdirSync(dir).length > 0) return
        fs.rmdirSync(dir)
      } catch {
        return
      }
      dir = path.dirname(dir)
    }
  }
}

vi.mock('@tauri-apps/api/core', () => ({
  invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
    const handler = commands[cmd]
    if (!handler) throw new Error(`unexpected command ${cmd}`)
    return handler(args)
  }
}))

const { applyWorkflow, undoWorkflowRun } = await import('../bridge/workflows')

function put(rel: string, text: string): void {
  fs.mkdirSync(path.dirname(abs(rel)), { recursive: true })
  fs.writeFileSync(abs(rel), text)
}

beforeEach(() => {
  root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'zn-applier-')))
  fs.mkdirSync(abs('inbox'), { recursive: true })
  fs.mkdirSync(abs('archive'), { recursive: true })
})

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true })
})

describe('workflow applier sidecars (#839)', () => {
  it('a move carries comments, creation date and edit record, and undo puts every byte back', async () => {
    put('inbox/A.md', '# A\n')
    put('.zennotes/comments/inbox/A.md.comments.json', '{"version":1,"comments":[]}')
    put('.zennotes/note-metadata/inbox/A.md.metadata.json', '{"version":1,"createdAt":42}\n')
    put('.zennotes/agent-edits/inbox/A.md.json', '{"version":1}\n')

    const receipt = await applyWorkflow({
      workflowId: 'w',
      ops: [{ kind: 'move', path: 'inbox/A.md', to: 'archive/Done' }]
    })
    expect(receipt.rolledBack).toBeUndefined()
    expect(readOrNull('archive/Done/A.md')).toBe('# A\n')
    expect(readOrNull('.zennotes/comments/archive/Done/A.md.comments.json')).toBe(
      '{"version":1,"comments":[]}'
    )
    expect(readOrNull('.zennotes/note-metadata/archive/Done/A.md.metadata.json')).toBe(
      '{"version":1,"createdAt":42}\n'
    )
    expect(readOrNull('.zennotes/agent-edits/archive/Done/A.md.json')).toBe('{"version":1}\n')
    expect(readOrNull('.zennotes/comments/inbox/A.md.comments.json')).toBeNull()

    const undo = await undoWorkflowRun(receipt.runId)
    expect(undo.restored).toBe(2)
    expect(readOrNull('inbox/A.md')).toBe('# A\n')
    expect(readOrNull('archive/Done/A.md')).toBeNull()
    expect(readOrNull('.zennotes/comments/inbox/A.md.comments.json')).toBe(
      '{"version":1,"comments":[]}'
    )
    expect(readOrNull('.zennotes/note-metadata/inbox/A.md.metadata.json')).toBe(
      '{"version":1,"createdAt":42}\n'
    )
    expect(readOrNull('.zennotes/agent-edits/inbox/A.md.json')).toBe('{"version":1}\n')
    expect(readOrNull('.zennotes/comments/archive/Done/A.md.comments.json')).toBeNull()
    // Nothing left behind that would block that folder name later.
    expect(fs.existsSync(abs('.zennotes/comments/archive'))).toBe(false)
  })

  it('records the creation date of a note that never had one before moving it', async () => {
    put('inbox/A.md', '# A\n')
    const receipt = await applyWorkflow({
      workflowId: 'w',
      ops: [{ kind: 'archive', path: 'inbox/A.md' }]
    })
    expect(receipt.rolledBack).toBeUndefined()
    expect(readOrNull('.zennotes/note-metadata/archive/A.md.metadata.json')).toBe(
      '{"version":1,"createdAt":1000}\n'
    )
  })

  it('refuses to move onto a name an earlier note left its comments on, and changes nothing', async () => {
    put('inbox/A.md', '# A\n')
    put('.zennotes/comments/archive/A.md.comments.json', '{"version":1,"comments":[]}')
    const receipt = await applyWorkflow({
      workflowId: 'w',
      ops: [{ kind: 'archive', path: 'inbox/A.md' }]
    })
    expect(receipt.rolledBack?.reason).toContain('Comments from an earlier note named “A”')
    expect(receipt.rolledBack?.reason).toContain('your vault is unchanged')
    // One full stop between the refusal and the rollback note, not two.
    expect(receipt.rolledBack?.reason).not.toContain('..')
    expect(readOrNull('inbox/A.md')).toBe('# A\n')
    expect(readOrNull('archive/A.md')).toBeNull()
  })

  it('a create over a leftover creation date drops it, and undo puts it back', async () => {
    put('.zennotes/note-metadata/inbox/New.md.metadata.json', '{"version":1,"createdAt":7}\n')
    const receipt = await applyWorkflow({
      workflowId: 'w',
      ops: [{ kind: 'create-note', path: 'inbox/New.md', body: '# New\n' }]
    })
    expect(receipt.rolledBack).toBeUndefined()
    expect(readOrNull('inbox/New.md')).toBe('# New\n')
    expect(readOrNull('.zennotes/note-metadata/inbox/New.md.metadata.json')).toBeNull()
    await undoWorkflowRun(receipt.runId)
    expect(readOrNull('inbox/New.md')).toBeNull()
    expect(readOrNull('.zennotes/note-metadata/inbox/New.md.metadata.json')).toBe(
      '{"version":1,"createdAt":7}\n'
    )
  })
})

describe('workflow applier and symlinked notes', () => {
  it('a moved link keeps pointing at its file, and undo moves the same link back', async () => {
    put('sources/X.md', '# X\n')
    fs.symlinkSync('../sources/X.md', abs('inbox/X.md'))
    const receipt = await applyWorkflow({
      workflowId: 'w',
      ops: [{ kind: 'move', path: 'inbox/X.md', to: 'inbox/Topics' }]
    })
    expect(receipt.rolledBack).toBeUndefined()
    expect(fs.readlinkSync(abs('inbox/Topics/X.md'))).toBe('../../sources/X.md')
    expect(readOrNull('inbox/Topics/X.md')).toBe('# X\n')

    await undoWorkflowRun(receipt.runId)
    expect(fs.lstatSync(abs('inbox/X.md')).isSymbolicLink()).toBe(true)
    expect(fs.readlinkSync(abs('inbox/X.md'))).toBe('../sources/X.md')
    expect(lexists(abs('inbox/Topics/X.md'))).toBe(false)
    // The file the link names was never the run's to touch.
    expect(readOrNull('sources/X.md')).toBe('# X\n')
  })
})
