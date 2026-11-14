/**
 * Upstream's logseq-import-run.test.ts, on the port's copy of the run
 * (src/bridge/imports/logseq-import-run.ts), over a Node stand-in for the
 * Rust host. The scan goes through the controller, which names the folder.
 */
import { promises as fs } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createImportHost, readNoteCreatedAt, type ImportHost } from './helpers/import-host'

let host: ImportHost

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args: Record<string, unknown> = {}) => host.invoke(command, args)
}))

const { runLogseqImport } = await import('../bridge/imports/logseq-import-run')
const { logseqImport } = await import('../bridge/imports/logseq-import')
type LogseqRunOptions = Parameters<typeof runLogseqImport>[0]

const ID = '6512a3b4-1c2d-4e5f-8a9b-0c1d2e3f4a5b'
const V = { source: '/logseq/source', root: '/logseq/vault', staging: '/logseq/staging' }

describe('Logseq import', () => {
  let scratch: string
  let graph: string
  let root: string
  let stagingDir: string

  const write = async (rel: string, content: string | Buffer): Promise<void> => {
    const abs = path.join(graph, rel)
    await fs.mkdir(path.dirname(abs), { recursive: true })
    await fs.writeFile(abs, content)
  }
  const read = (rel: string): Promise<string> => fs.readFile(path.join(root, rel), 'utf8')
  const exists = (rel: string): Promise<boolean> => fs.access(path.join(root, rel)).then(() => true, () => false)

  beforeEach(async () => {
    scratch = await fs.mkdtemp(path.join(os.tmpdir(), 'zen-logseq-'))
    graph = path.join(scratch, 'My Graph')
    root = path.join(scratch, 'vault')
    stagingDir = path.join(scratch, 'staging')
    host = createImportHost({
      '/logseq/source': graph,
      '/logseq/picked': graph,
      '/logseq/vault': root,
      '/logseq/staging': stagingDir
    })
    host.handlers.import_pick = () => host.places['/logseq/picked']
    host.handlers.import_adopt = () => undefined
    await fs.mkdir(path.join(root, 'inbox'), { recursive: true })
    await write('logseq/config.edn', '{:file/name-format :triple-lowbar\n :journal/page-title-format "MMM do, yyyy"}')
    await write('logseq/custom.css', 'body {}')
    await write('logseq/bak/pages/old.md', '- backup')
    await write('pages/Projects___ZenNotes.md', `tags:: work\n\n- The idea\n  id:: ${ID}\n- TODO [#A] Ship\n  DEADLINE: <2024-01-20 Sat>\n- ![shot](../assets/shot.png)\n`)
    await write('pages/Home.md', `- See ((${ID})) on [[Projects/ZenNotes]], [[Jan 15th, 2024]]\n- {{query (task TODO)}}\n`)
    await write('pages/agenda.org', '* TODO org thing\n')
    await write('journals/2024_01_15.md', '- NOW Working\n')
    await write('journals/2024_01_16.md', '- Second day\n')
    await write('assets/shot.png', Buffer.from([1, 2, 3]))
    await write('whiteboards/board.edn', '{}')
    await write('.recycle/gone.md', '- deleted')
  })

  afterEach(async () => {
    await fs.rm(scratch, { recursive: true, force: true })
  })

  const run = (overrides: Partial<LogseqRunOptions> = {}) =>
    runLogseqImport({
      source: V.source,
      stagingDir: V.staging,
      root: V.root,
      notesRoot: `${V.root}/inbox`,
      existingNotes: [],
      folder: 'Logseq',
      bullets: false,
      includeTasks: true,
      onProgress: () => undefined,
      today: '2026-11-09',
      ...overrides
    })

  it('scans what a graph holds', async () => {
    const scan = await logseqImport.pick()
    expect(scan).toMatchObject({
      name: 'My Graph',
      path: graph,
      isLogseq: true,
      pages: 2,
      journals: 2,
      orgPages: 1,
      files: 2,
      journalDates: ['2024-01-15', '2024-01-16']
    })
  })

  it('refuses a folder that is not a graph, and does not take it', async () => {
    const empty = path.join(scratch, 'empty')
    await fs.mkdir(empty)
    host.places['/logseq/picked'] = empty
    await expect(logseqImport.pick()).rejects.toThrow("isn't a Logseq graph")
    expect(host.calls.map((call) => call.command)).not.toContain('import_adopt')
  })

  it('copies the graph into one folder and journals into daily notes, leaving the graph as it was', async () => {
    const before = await fs.readFile(path.join(graph, 'pages/Home.md'), 'utf8')
    await fs.mkdir(path.join(root, 'inbox/Daily Notes'), { recursive: true })
    await fs.writeFile(path.join(root, 'inbox/Daily Notes/2024-01-16.md'), 'mine\n')
    const result = await run({
      existingNotes: ['inbox/Daily Notes/2024-01-16.md'],
      dailyNotes: {
        '2024-01-15': { subpath: 'Daily Notes', title: '2024-01-15', exists: false },
        '2024-01-16': { subpath: 'Daily Notes', title: '2024-01-16', exists: false }
      }
    })
    expect(result).toMatchObject({
      folder: 'inbox/Logseq',
      reportPath: 'inbox/Logseq/Import Report.md',
      pages: 3,
      journals: 2,
      files: 2,
      kept: 3
    })

    expect(await read('inbox/Daily Notes/2024-01-15.md')).toBe('- [/] Working\n')
    // That day already had a daily note: it is untouched and the journal stays in the folder.
    expect(await read('inbox/Daily Notes/2024-01-16.md')).toBe('mine\n')
    expect(await read('inbox/Logseq/Journals/2024-01-16.md')).toBe('Second day\n')

    expect(await read('inbox/Logseq/Projects/ZenNotes.md')).toBe(
      `---\ntags: [work]\n---\n\nThe idea ^${ID}\n\n- [ ] Ship !high due:2024-01-20\n\n![shot](../assets/shot.png)\n`
    )
    expect(await read('inbox/Logseq/Home.md')).toBe(
      `See [[ZenNotes^${ID}|The idea]] on [[ZenNotes|Projects/ZenNotes]], [[2024-01-15|Jan 15th, 2024]]\n\n\`\`\`logseq\n{{query (task TODO)}}\n\`\`\`\n`
    )
    expect(await read('inbox/Logseq/agenda.md')).toContain('```org\n* TODO org thing\n```')
    expect(await fs.readFile(path.join(root, 'inbox/Logseq/assets/shot.png'))).toEqual(Buffer.from([1, 2, 3]))
    expect(await exists('inbox/Logseq/whiteboards/board.edn')).toBe(true)
    expect(await exists('inbox/Logseq/logseq')).toBe(false)
    expect(await exists('inbox/Logseq/.recycle')).toBe(false)

    const report = await read('inbox/Logseq/Import Report.md')
    expect(report).toContain('## Journals kept here')
    expect(report).toContain('- 1 query')
    expect(report).toContain('- 1 Org mode page')
    expect(report).toContain('- 1 whiteboard')

    expect(await fs.readFile(path.join(graph, 'pages/Home.md'), 'utf8')).toBe(before)
    expect(await exists('../staging')).toBe(false)
    const created = await readNoteCreatedAt(root, 'inbox/Daily Notes/2024-01-15.md', 0)
    expect(new Date(created).getDate()).toBe(15)

    // A daily note goes in only where nothing is: an exclusive write.
    const daily = host.calls.find(
      (call) => call.command === 'import_fs_write_text' && call.args.path === `${V.root}/inbox/Daily Notes/2024-01-15.md`
    )
    expect(daily?.args.exclusive).toBe(true)
  })

  it('puts a journal in the folder when a daily note appears for its day while importing', async () => {
    // The vault gains that day's note between the check and the write.
    const real = host.invoke
    host.invoke = async (command, args) => {
      if (command === 'import_fs_move_into_vault') {
        await fs.mkdir(path.join(root, 'inbox/Daily Notes'), { recursive: true })
        await fs.writeFile(path.join(root, 'inbox/Daily Notes/2024-01-15.md'), 'written meanwhile\n')
      }
      return real(command, args)
    }
    const result = await run({
      dailyNotes: { '2024-01-15': { subpath: 'Daily Notes', title: '2024-01-15', exists: false } }
    })
    expect(await read('inbox/Daily Notes/2024-01-15.md')).toBe('written meanwhile\n')
    expect(await read('inbox/Logseq/Journals/2024-01-15.md')).toBe('- [/] Working\n')
    expect(await read('inbox/Logseq/Import Report.md')).toContain('## Journals written while importing')
    expect(result.issues).toBeGreaterThan(0)
  })

  it('never merges into a folder that exists', async () => {
    await fs.mkdir(path.join(root, 'inbox/Logseq'))
    await fs.writeFile(path.join(root, 'inbox/Logseq/Home.md'), 'already here\n')
    const result = await run()
    expect(result.folder).toBe('inbox/Logseq 2')
    expect(await read('inbox/Logseq/Home.md')).toBe('already here\n')
    // Without daily-note places, journals stay in the folder.
    expect(await read('inbox/Logseq 2/Journals/2024-01-15.md')).toBe('- [/] Working\n')
  })

  it('refuses a folder name or daily-note place that leaves the notes area', async () => {
    await expect(run({ folder: '../outside' })).rejects.toThrow('Choose a folder name')
    const result = await run({
      dailyNotes: { '2024-01-15': { subpath: '../../escape', title: '2024-01-15', exists: false } }
    })
    expect(await exists('../escape/2024-01-15.md')).toBe(false)
    expect(await exists('inbox/Logseq/Journals/2024-01-15.md')).toBe(true)
    expect(result.journals).toBe(2)
  })

  it('refuses a graph that holds the vault', async () => {
    await expect(run({ source: '/logseq' })).rejects.toThrow('holds this vault')
  })
})
