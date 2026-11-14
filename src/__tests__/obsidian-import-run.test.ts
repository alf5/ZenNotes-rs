/**
 * Upstream's obsidian-import-run.test.ts, on the port's copy of the run
 * (src/bridge/imports/obsidian-import-run.ts). The run sees only virtual
 * paths; a Node stand-in for the Rust host maps them onto temp folders.
 * The scan goes through the controller, which names the real folder.
 */
import { promises as fs } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { NotionAbortError } from '@shared/notion-import/client'
import { createImportHost, readNoteCreatedAt, type ImportHost } from './helpers/import-host'

let host: ImportHost

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args: Record<string, unknown> = {}) => host.invoke(command, args)
}))

const { runObsidianImport } = await import('../bridge/imports/obsidian-import-run')
const { obsidianImport } = await import('../bridge/imports/obsidian-import')
type ObsidianRunOptions = Parameters<typeof runObsidianImport>[0]

const V = { source: '/obsidian/source', root: '/obsidian/vault', staging: '/obsidian/staging' }

describe('Obsidian import', () => {
  let scratch: string
  let source: string
  let root: string
  let stagingDir: string

  const write = async (rel: string, content: string | Buffer): Promise<void> => {
    const abs = path.join(source, rel)
    await fs.mkdir(path.dirname(abs), { recursive: true })
    await fs.writeFile(abs, content)
  }
  const inImport = (rel: string): string => path.join(root, 'Obsidian Import', rel)
  const exists = (abs: string): Promise<boolean> => fs.access(abs).then(() => true, () => false)

  beforeEach(async () => {
    scratch = await fs.mkdtemp(path.join(os.tmpdir(), 'zen-obsidian-'))
    source = path.join(scratch, 'My Vault')
    root = path.join(scratch, 'vault')
    stagingDir = path.join(scratch, 'staging')
    host = createImportHost({
      '/obsidian/source': source,
      '/obsidian/picked': source,
      '/obsidian/vault': root,
      '/obsidian/staging': stagingDir
    })
    host.handlers.import_pick = () => source
    host.handlers.import_adopt = () => undefined
    await fs.mkdir(root, { recursive: true })
    await write('.obsidian/app.json', '{}')
    await write('.obsidian/daily-notes.json', '{"folder":"Journal"}')
    await write('.trash/old.md', 'deleted')
    await write('Home.md', 'Unchanged [[Plan]] ![[photo.png]]\n')
    await write('Projects/Plan.md', '---\ntags: [a]\n---\nThe plan.\n- [ ] do it\n')
    await write('Projects/Notes.md', 'Project notes\n')
    await write('Areas/Notes.md', 'See [[Notes]]\n')
    await write('Journal/2026-01-01.md', 'Day\n')
    await write('Private/Key.md', 'secret\n')
    await write('Private/used.png', Buffer.from([9]))
    await write('Private/unused.png', Buffer.from([8]))
    await write('attachments/photo.png', Buffer.from([1, 2, 3]))
    await write('board.canvas', '{}')
    await write('Dash.md', '```dataview\nLIST\n```\n')
  })

  afterEach(async () => {
    vi.restoreAllMocks()
    await fs.rm(scratch, { recursive: true, force: true })
  })

  const run = (overrides: Partial<ObsidianRunOptions> = {}) =>
    runObsidianImport({
      source: V.source,
      stagingDir: V.staging,
      root: V.root,
      notesRoot: V.root,
      existingNotes: [],
      existingAssets: [],
      excluded: [],
      includeTasks: true,
      onProgress: () => undefined,
      ...overrides
    })

  it('describes a vault, leaving out Obsidian’s own folders', async () => {
    const scan = await obsidianImport.pick()
    expect(scan?.isObsidian).toBe(true)
    expect(scan?.name).toBe('My Vault')
    expect(scan?.path).toBe(source)
    expect(scan?.notes).toBe(7)
    expect(scan?.files).toBe(4)
    const titles = scan?.tree.map((item) => item.title)
    expect(titles).not.toContain('.obsidian')
    expect(titles).not.toContain('.trash')
    const projects = scan?.tree.find((item) => item.title === 'Projects')
    expect(projects?.kind).toBe('folder')
    expect(projects?.noteCount).toBe(2)
    expect(scan?.tree.find((item) => item.title === 'attachments')?.fileCount).toBe(1)
    // The folder became the source before it was read, as upstream's does.
    const commands = host.calls.map((call) => call.command)
    expect(commands.indexOf('import_adopt')).toBeLessThan(commands.indexOf('import_fs_read_dir'))
  })

  it('says when a folder is not an Obsidian vault', async () => {
    await fs.rm(path.join(source, '.obsidian'), { recursive: true })
    expect((await obsidianImport.pick())?.isObsidian).toBe(false)
  })

  it('answers null when the dialog is cancelled', async () => {
    host.handlers.import_pick = () => null
    await expect(obsidianImport.pick()).resolves.toBeNull()
  })

  it('copies the structure, and leaves a note nothing changed byte for byte', async () => {
    const result = await run()
    expect(result.folder).toBe('Obsidian Import')
    expect(result.pages).toBe(7)
    expect(await fs.readFile(inImport('Home.md'), 'utf8')).toBe('Unchanged [[Plan]] ![[photo.png]]\n')
    expect(await fs.readFile(inImport('attachments/photo.png'))).toEqual(Buffer.from([1, 2, 3]))
    expect(await exists(inImport('.obsidian'))).toBe(false)
    expect(await exists(inImport('.trash'))).toBe(false)
    await expect(fs.access(stagingDir)).rejects.toThrow()
    // An unchanged note is copied by the host, not rewritten.
    const copied = host.calls.filter((call) => call.command === 'import_fs_copy').map((call) => call.args.from)
    expect(copied).toContain('/obsidian/source/Home.md')
  })

  it('never changes the original vault', async () => {
    const before = await fs.readFile(path.join(source, 'Areas/Notes.md'), 'utf8')
    await run()
    expect(await fs.readFile(path.join(source, 'Areas/Notes.md'), 'utf8')).toBe(before)
    expect(await exists(path.join(source, 'Obsidian Import'))).toBe(false)
    const writes = host.calls.filter((call) =>
      ['import_fs_write_text', 'import_fs_append_text', 'import_fs_mkdir', 'import_fs_rm', 'import_fs_rename'].includes(
        call.command
      )
    )
    for (const call of writes) expect(String(call.args.path ?? call.args.to)).not.toMatch(/^\/obsidian\/source/)
  })

  it('pins a link whose title is shared, to the note Obsidian would open', async () => {
    await run()
    expect(await fs.readFile(inImport('Areas/Notes.md'), 'utf8')).toBe('See [[Obsidian Import/Areas/Notes|Notes]]\n')
  })

  it('pins a link that would clash with a note already in the vault', async () => {
    await run({ existingNotes: ['Plan.md'] })
    expect(await fs.readFile(inImport('Home.md'), 'utf8')).toContain('[[Obsidian Import/Projects/Plan|Plan]]')
  })

  it('uses the vault’s notes folder, and links by the path from its root', async () => {
    const notesRoot = path.join(root, 'inbox')
    const result = await run({ notesRoot: `${V.root}/inbox` })
    expect(result.folder).toBe('inbox/Obsidian Import')
    expect(await fs.readFile(path.join(notesRoot, 'Obsidian Import/Areas/Notes.md'), 'utf8')).toContain(
      '[[inbox/Obsidian Import/Areas/Notes|Notes]]'
    )
  })

  it('leaves out what was unticked, but keeps a file a note that came over uses', async () => {
    await write('Home.md', 'Uses ![[used.png]]\n')
    const result = await run({ excluded: ['Private'] })
    expect(await exists(inImport('Private/Key.md'))).toBe(false)
    expect(await exists(inImport('Private/unused.png'))).toBe(false)
    expect(await exists(inImport('Private/used.png'))).toBe(true)
    expect(result.pages).toBe(6)
  })

  it('leaves out a single unticked note', async () => {
    await run({ excluded: ['Projects/Plan.md'] })
    expect(await exists(inImport('Projects/Plan.md'))).toBe(false)
    expect(await exists(inImport('Projects/Notes.md'))).toBe(true)
  })

  it('keeps to-dos in Tasks unless asked not to, changing the note only then', async () => {
    await run()
    expect(await fs.readFile(inImport('Projects/Plan.md'), 'utf8')).toBe('---\ntags: [a]\n---\nThe plan.\n- [ ] do it\n')
    await fs.rm(path.join(root, 'Obsidian Import'), { recursive: true })
    await run({ includeTasks: false })
    expect(await fs.readFile(inImport('Projects/Plan.md'), 'utf8')).toBe('---\ntags: [a]\ntasks: false\n---\nThe plan.\n- [ ] do it\n')
  })

  it('keeps file dates', async () => {
    const when = new Date('2021-05-06T07:08:09.000Z')
    await fs.utimes(path.join(source, 'Home.md'), when, when)
    await run()
    expect((await fs.stat(inImport('Home.md'))).mtimeMs).toBe(when.getTime())
    expect(await readNoteCreatedAt(root, 'Obsidian Import/Home.md', 0)).toBeGreaterThan(0)
  })

  it('reports what will not work here, and where to point Settings', async () => {
    const result = await run()
    const report = await fs.readFile(inImport('Import Report.md'), 'utf8')
    expect(report).toContain('board.canvas')
    expect(report).toContain('1 Dataview query stays a code block')
    expect(report).toContain('Daily notes: Obsidian Import/Journal')
    expect(report).toContain('The original vault was not changed.')
    expect(result.issues).toBe(2)
  })

  it('only points at a settings folder that came over', async () => {
    await write('.obsidian/templates.json', '{"folder":"Never made"}')
    await run({ excluded: ['Journal'] })
    const report = await fs.readFile(inImport('Import Report.md'), 'utf8')
    expect(report).not.toContain('Daily notes:')
    expect(report).not.toContain('Templates:')
    expect(report).not.toContain('Settings to carry over')
  })

  it('makes a second folder rather than touch an earlier import', async () => {
    await run()
    await fs.writeFile(inImport('Mine.md'), 'mine')
    const second = await run()
    expect(second.folder).toBe('Obsidian Import 2')
    expect(await fs.readFile(inImport('Mine.md'), 'utf8')).toBe('mine')
    expect(await fs.readFile(path.join(root, 'Obsidian Import 2/Areas/Notes.md'), 'utf8')).toContain(
      '[[Obsidian Import 2/Areas/Notes|Notes]]'
    )
  })

  it('refuses to copy a vault into itself, either way round', async () => {
    // On real paths the host refuses this at pick and start (paths.rs); the
    // run's own check, on its virtual paths, still holds.
    await expect(run({ root: V.source, notesRoot: V.source })).rejects.toThrow('holds this vault')
    await expect(run({ source: path.posix.dirname(V.root) })).rejects.toThrow('holds this vault')
  })

  it('does not follow a symlink out of the vault', async () => {
    const outside = path.join(scratch, 'outside')
    await fs.mkdir(outside)
    await fs.writeFile(path.join(outside, 'Leak.md'), 'private')
    await fs.symlink(outside, path.join(source, 'Linked'))
    await run()
    expect(await exists(inImport('Linked'))).toBe(false)
  })

  it('leaves the vault alone when stopped', async () => {
    const controller = new AbortController()
    let seen = 0
    const stopped = run({
      signal: controller.signal,
      onProgress: (progress) => {
        if (progress.phase === 'writing' && ++seen === 2) controller.abort()
      }
    })
    await expect(stopped).rejects.toBeInstanceOf(NotionAbortError)
    expect(await fs.readdir(root)).toEqual([])
  })
})
