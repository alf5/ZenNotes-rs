import { describe, expect, it } from 'vitest'
import { findHeadingForAnchor, headingSlug } from './heading-anchor'

const headings = [
  { text: 'Heading link test' },
  { text: 'Section One' },
  { text: 'Section Three' },
  { text: 'What’s new in **2.60**?' },
  { text: 'Überblick & Ziele' }
]

const find = (anchor: string): string | undefined => findHeadingForAnchor(headings, anchor)?.text

describe('findHeadingForAnchor', () => {
  it('matches the heading text, case-insensitively, as wikilinks always did', () => {
    expect(find('Section Three')).toBe('Section Three')
    expect(find('section three')).toBe('Section Three')
  })

  it('matches the URL-encoded text a Markdown link has to use for spaces', () => {
    expect(find('Section%20Three')).toBe('Section Three')
  })

  it('matches the GitHub-style slug Markdown tools generate', () => {
    expect(find('section-three')).toBe('Section Three')
    expect(find('whats-new-in-260')).toBe('What’s new in **2.60**?')
    expect(find('überblick--ziele')).toBe('Überblick & Ziele')
  })

  it('prefers an exact name over a slug that collides with it', () => {
    const both = [{ text: 'Section Three' }, { text: 'section-three' }]
    expect(findHeadingForAnchor(both, 'section-three')?.text).toBe('section-three')
  })

  it('finds nothing for an anchor no heading carries', () => {
    expect(find('section-four')).toBeUndefined()
    expect(find('   ')).toBeUndefined()
    expect(find('%E0%A4%A')).toBeUndefined()
  })
})

describe('headingSlug', () => {
  it('lowercases, drops punctuation and hyphenates spaces', () => {
    expect(headingSlug('  Section Three ')).toBe('section-three')
    expect(headingSlug('A `code` & a [link](x)')).toBe('a-code--a-linkx')
  })
})
