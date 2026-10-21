import { describe, expect, it } from 'vitest'
import { DEMO_TOUR_ASSETS, DEMO_TOUR_NOTES } from '@shared/demo-tour-data'
import {
  currentDemoTourPath,
  DEMO_TOUR_START_PATH,
  demoTourNestedDirs,
  isDemoTourStartPath
} from '@shared/demo-tour'
import { formDirFromCsvPath, formTitleFromCsvPath } from '@shared/databases'
import { extractWikilinkTargets, resolveWikilinkTarget } from './wikilinks'

// Built from its code point so this file never contains the character itself.
const LONG_DASH = String.fromCharCode(0x2014)

// The tour's own example of following a link to a note that does not exist yet.
const INTENTIONALLY_MISSING = new Set(['A Future Note'])

const tourNotes = DEMO_TOUR_NOTES.map((note) => ({
  path: note.path,
  title: note.path.slice(note.path.lastIndexOf('/') + 1).replace(/\.md$/, ''),
  folder: 'inbox' as const
}))

// Files the tour ships beside its notes answer to their file name, the way
// `![[Tour Sketch.excalidraw]]` finds the drawing, and a database to its title,
// the way `[[Reading List]]` opens `Reading List.base`.
const assetTargets = new Set(
  DEMO_TOUR_ASSETS.flatMap((asset) => [
    asset.path,
    asset.path.slice(asset.path.lastIndexOf('/') + 1),
    ...(formDirFromCsvPath(asset.path) ? [formTitleFromCsvPath(asset.path)] : [])
  ])
)

describe('demo tour vault', () => {
  it('resolves every wikilink and embed to a note or file the tour ships', () => {
    const unresolved: string[] = []
    let resolved = 0
    for (const note of DEMO_TOUR_NOTES) {
      for (const target of extractWikilinkTargets(note.body)) {
        if (INTENTIONALLY_MISSING.has(target)) continue
        if (assetTargets.has(target)) continue
        if (resolveWikilinkTarget(tourNotes, target)) resolved++
        else unresolved.push(`${note.path}: [[${target}]]`)
      }
    }
    expect(unresolved).toEqual([])
    expect(resolved).toBeGreaterThan(0)
  })

  it('opens on a note it ships', () => {
    expect(DEMO_TOUR_NOTES.map((note) => note.path)).toContain(DEMO_TOUR_START_PATH)
  })

  it('names and writes every file without the long dash', () => {
    for (const file of [...DEMO_TOUR_NOTES, ...DEMO_TOUR_ASSETS]) {
      expect(file.path).not.toContain(LONG_DASH)
      expect(file.body).not.toContain(LONG_DASH)
    }
  })
})

describe('demo tour notes under their older names', () => {
  const olderStart = `inbox/demo/00 ${LONG_DASH} Start Here.md`

  it('maps an older name to the current one', () => {
    expect(currentDemoTourPath(olderStart)).toBe(DEMO_TOUR_START_PATH)
    expect(currentDemoTourPath(`inbox/demo/05b ${LONG_DASH} Math Diagrams.md`)).toBe(
      'inbox/demo/05b Math Diagrams.md'
    )
  })

  it('leaves current names and unrelated paths alone', () => {
    for (const path of [
      DEMO_TOUR_START_PATH,
      'inbox/demo/11 Workspace, Search, and Views.md',
      'inbox/demo/01 - Markdown Basics.md',
      // Only the old separator itself: another character is someone's own note.
      'inbox/demo/00 · Start Here.md',
      `inbox/notes/00 ${LONG_DASH} Start Here.md`
    ]) {
      expect(currentDemoTourPath(path)).toBe(path)
    }
  })

  it('recognizes the start note under either name', () => {
    expect(isDemoTourStartPath(DEMO_TOUR_START_PATH)).toBe(true)
    expect(isDemoTourStartPath(olderStart)).toBe(true)
    expect(isDemoTourStartPath('inbox/demo/01 Markdown Basics.md')).toBe(false)
  })
})

describe('folders the tour ships inside its own folder', () => {
  it('lists them deepest first, so each is empty when its turn comes', () => {
    expect(
      demoTourNestedDirs([
        'zennotes-demo-card.svg',
        'inbox/demo/Tour Sketch.excalidraw',
        'inbox/demo/Reading List.base/data.csv',
        'inbox/demo/Reading List.base/pages/One.md',
        'inbox/demo/Reading List.base/schema.json'
      ])
    ).toEqual(['inbox/demo/Reading List.base/pages', 'inbox/demo/Reading List.base'])
  })

  it('covers the sample database the tour ships', () => {
    expect(demoTourNestedDirs(DEMO_TOUR_ASSETS.map((asset) => asset.path))).toContain(
      'inbox/demo/Reading List.base'
    )
  })
})
