import { describe, expect, it } from 'vitest'
import { pastedImageFilename } from './pasted-image'

const at = new Date(2026, 7, 6, 9, 5, 3)

describe('pastedImageFilename', () => {
  it("names a clipboard bitmap by the time, not Chromium's image.png (#775)", () => {
    const now = new Date(2026, 9, 19, 13, 45, 12)
    expect(pastedImageFilename({ mimeType: 'image/png', suggestedName: 'image.png' }, now)).toBe(
      'Pasted Image 2026-10-19 134512.png'
    )
    expect(pastedImageFilename({ mimeType: 'image/jpeg', suggestedName: 'Image.JPEG' }, now)).toBe(
      'Pasted Image 2026-10-19 134512.jpeg'
    )
  })

  it('keeps the name of a real file that merely starts with image (#775)', () => {
    const now = new Date(2026, 9, 19, 13, 45, 12)
    expect(pastedImageFilename({ mimeType: 'image/png', suggestedName: 'image-2.png' }, now)).toBe(
      'image-2.png'
    )
    expect(pastedImageFilename({ mimeType: 'image/png', suggestedName: 'Diagram.png' }, now)).toBe(
      'Diagram.png'
    )
  })

  it('falls back to the timestamped name when nothing is suggested', () => {
    expect(pastedImageFilename({ mimeType: 'image/png' }, at)).toBe(
      'Pasted Image 2026-08-06 090503.png'
    )
  })

  it('keeps a clean suggested name and its image extension', () => {
    expect(
      pastedImageFilename({ mimeType: 'image/png', suggestedName: 'diagram.webp' }, at)
    ).toBe('diagram.webp')
  })

  it('scrubs the characters that break a wikilink embed', () => {
    // [ ] and # survive the server's own filename cleaning, and a raw
    // "![[assets/diagram [v2] #3.png]]" renders as a broken embed.
    expect(
      pastedImageFilename({ mimeType: 'image/png', suggestedName: 'diagram [v2] #3.png' }, at)
    ).toBe('diagram -v2- -3.png')
  })

  it('derives the extension from the mime type when the name has none', () => {
    expect(
      pastedImageFilename({ mimeType: 'image/webp', suggestedName: 'shot' }, at)
    ).toBe('shot.webp')
  })

  it('drops path components from the suggested name', () => {
    expect(
      pastedImageFilename({ mimeType: 'image/png', suggestedName: '../secrets/pic.png' }, at)
    ).toBe('pic.png')
  })

  it('rejects non-image clipboard items', () => {
    expect(() => pastedImageFilename({ mimeType: 'application/pdf' }, at)).toThrow()
  })
})
