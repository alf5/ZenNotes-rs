import { describe, expect, it } from 'vitest'
import { planRecordPages, recordPageCandidates } from './database-records'
import type { DatabaseDoc } from './databases'

const doc: DatabaseDoc = {
  version: 1,
  path: 'Books.base/data.csv',
  title: 'Books',
  idFieldId: 'id',
  activeViewId: 'table',
  views: [],
  fields: [
    { id: 'id', name: 'ID', type: 'text' },
    { id: 'title', name: 'Title', type: 'text' }
  ],
  rows: [
    { id: 'a', cells: { id: 'a', title: 'Dune' } },
    { id: 'b', cells: { id: 'b', title: 'Emma' } },
    { id: 'c', cells: { id: 'c', title: 'Kindred' } },
    { id: 'd', cells: { id: 'd', title: '   ' } }
  ],
  pages: { a: 'Books.base/Dune.md', b: 'Books.base/Emma.md' }
}

describe('planRecordPages', () => {
  it('plans a page for records without one, or whose page is gone, in row order', () => {
    const plan = planRecordPages(doc, (path) => path === 'Books.base/Dune.md')
    expect(plan.missing.map((r) => r.id)).toEqual(['b', 'c'])
    expect(plan.adopt).toEqual([])
    expect(plan.existing).toBe(1)
    expect(plan.untitled).toBe(1)
  })

  it('plans nothing for a database whose titled records all have pages', () => {
    const plan = planRecordPages({ ...doc, rows: doc.rows.slice(0, 2) }, () => true)
    expect(plan).toEqual({ missing: [], adopt: [], existing: 2, untitled: 0 })
  })

  it('leaves out every record when the database has no title field', () => {
    const plan = planRecordPages({ ...doc, fields: [doc.fields[0]], pages: {} }, () => false)
    expect(plan.missing).toEqual([])
    expect(plan.untitled).toBe(4)
  })
})

describe('planRecordPages with page files already there', () => {
  it('links an unmapped file named after the record, in the folder or pages/', () => {
    const files = new Set(['Books.base/Dune.md', 'Books.base/pages/Kindred.md'])
    const plan = planRecordPages({ ...doc, pages: { a: 'Books.base/Dune.md' } }, (p) => files.has(p))
    expect(plan.adopt.map(({ row, page }) => [row.id, page])).toEqual([['c', 'Books.base/pages/Kindred.md']])
    expect(plan.missing.map((r) => r.id)).toEqual(['b'])
  })

  it('gives a file to the first of two records that share a title', () => {
    const twins = { ...doc, rows: [doc.rows[0], { id: 'e', cells: { id: 'e', title: 'Dune' } }], pages: {} }
    const plan = planRecordPages(twins, (p) => p === 'Books.base/Dune.md')
    expect(plan.adopt.map(({ row }) => row.id)).toEqual(['a'])
    expect(plan.missing.map((r) => r.id)).toEqual(['e'])
  })

  it("names candidates the way the app, the server and zn name a page, and as the title is", () => {
    expect(recordPageCandidates(doc, 'What: a "guide"?')).toEqual([
      'Books.base/What- a -guide--.md',
      'Books.base/What: a "guide"?.md',
      'Books.base/pages/What- a -guide--.md',
      'Books.base/pages/What: a "guide"?.md'
    ])
    // A slash in a title never makes a path of it.
    expect(recordPageCandidates(doc, 'A/B')).toEqual(['Books.base/A-B.md', 'Books.base/pages/A-B.md'])
  })
})

