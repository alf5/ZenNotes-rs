import { describe, it, expect } from 'vitest'
import {
  parseEmbedSizeHint,
  resolveExcalidrawEmbedPath,
  splitEmbedLabel
} from './excalidraw-preview'

describe('parseEmbedSizeHint', () => {
  it('parses a bare width', () => {
    expect(parseEmbedSizeHint('600')).toEqual({ width: 600, height: undefined })
  })

  it('parses width x height', () => {
    expect(parseEmbedSizeHint('600x400')).toEqual({ width: 600, height: 400 })
  })

  it('returns null for empty or undefined input', () => {
    expect(parseEmbedSizeHint(null)).toBeNull()
    expect(parseEmbedSizeHint(undefined)).toBeNull()
    expect(parseEmbedSizeHint('')).toBeNull()
  })

  it('returns null for non-numeric input', () => {
    expect(parseEmbedSizeHint('wide')).toBeNull()
    expect(parseEmbedSizeHint('abcx123')).toBeNull()
  })

  it('trims whitespace before matching', () => {
    expect(parseEmbedSizeHint('  800  ')).toEqual({ width: 800, height: undefined })
  })

  it('rejects zero dimensions instead of half-applying them', () => {
    // `|0x300` used to eat the caption, skip the falsy width downstream, and
    // set only the height, distorting the image.
    expect(parseEmbedSizeHint('0')).toBeNull()
    expect(parseEmbedSizeHint('0x300')).toBeNull()
    expect(parseEmbedSizeHint('300x0')).toBeNull()
  })
})

describe('splitEmbedLabel (#570)', () => {
  it('treats a pure size label as hint only in the wikilink form', () => {
    expect(splitEmbedLabel('100x50', 'wikilink')).toEqual({
      alt: '',
      size: { width: 100, height: 50 }
    })
  })

  it('keeps a purely numeric markdown alt as the caption', () => {
    // `![2024](chart.png)`: 2024 is a caption (a year), not a resize to
    // 2024px. Sizing a markdown image needs the pipe: `![|2024](chart.png)`.
    expect(splitEmbedLabel('2024', 'markdown')).toEqual({ alt: '2024', size: null })
    expect(splitEmbedLabel('|2024', 'markdown')).toEqual({
      alt: '',
      size: { width: 2024, height: undefined }
    })
  })

  it('splits a trailing hint off a caption', () => {
    expect(splitEmbedLabel('cognitive web|300', 'markdown')).toEqual({
      alt: 'cognitive web',
      size: { width: 300, height: undefined }
    })
  })

  it('keeps pipes inside the caption and consumes only the last segment', () => {
    expect(splitEmbedLabel('a|b|600x400', 'wikilink')).toEqual({
      alt: 'a|b',
      size: { width: 600, height: 400 }
    })
  })

  it('leaves captions without a valid hint alone', () => {
    expect(splitEmbedLabel('just a caption', 'wikilink')).toEqual({
      alt: 'just a caption',
      size: null
    })
    expect(splitEmbedLabel('trailing|600x', 'wikilink')).toEqual({
      alt: 'trailing|600x',
      size: null
    })
    expect(splitEmbedLabel('caption|0x300', 'wikilink')).toEqual({
      alt: 'caption|0x300',
      size: null
    })
    expect(splitEmbedLabel('', 'wikilink')).toEqual({ alt: '', size: null })
  })
})

describe('resolveExcalidrawEmbedPath', () => {
  const notes = [
    'inbox/My Drawing.excalidraw',
    'Drawings/Architecture.excalidraw',
    'refs/Obsidian Drawing.excalidraw.md',
    'inbox/notes.md'
  ]

  it('finds an exact path match', () => {
    expect(resolveExcalidrawEmbedPath(notes, 'inbox/My Drawing.excalidraw')).toBe(
      'inbox/My Drawing.excalidraw'
    )
  })

  it('resolves by suffix when the full path is given', () => {
    expect(resolveExcalidrawEmbedPath(notes, 'Drawings/Architecture.excalidraw')).toBe(
      'Drawings/Architecture.excalidraw'
    )
  })

  it('resolves a bare filename to its full path', () => {
    expect(resolveExcalidrawEmbedPath(notes, 'My Drawing.excalidraw')).toBe(
      'inbox/My Drawing.excalidraw'
    )
  })

  it('resolves by title without extension', () => {
    expect(resolveExcalidrawEmbedPath(notes, 'Architecture')).toBe(
      'Drawings/Architecture.excalidraw'
    )
  })

  it('resolves Obsidian .excalidraw.md files', () => {
    expect(resolveExcalidrawEmbedPath(notes, 'Obsidian Drawing.excalidraw.md')).toBe(
      'refs/Obsidian Drawing.excalidraw.md'
    )
  })

  it('returns null for an empty target', () => {
    expect(resolveExcalidrawEmbedPath(notes, '')).toBeNull()
    expect(resolveExcalidrawEmbedPath(notes, '  ')).toBeNull()
  })

  it('returns null when no match exists', () => {
    expect(resolveExcalidrawEmbedPath(notes, 'nonexistent.excalidraw')).toBeNull()
  })
})
