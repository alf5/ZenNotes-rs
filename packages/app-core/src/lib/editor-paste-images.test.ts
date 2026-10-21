// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import {
  editorImagePaste,
  isClipboardImageFile,
  pasteImageFilesIntoEditor,
  pastedImageInputFromFile,
  readSystemClipboard
} from './editor-paste-images'

function stubClipboard(clipboard: Partial<Clipboard> | undefined): void {
  Object.defineProperty(navigator, 'clipboard', { value: clipboard, configurable: true })
}

function clipboardItem(parts: Record<string, BlobPart>): ClipboardItem {
  return {
    types: Object.keys(parts),
    getType: async (type: string) => new Blob([parts[type]], { type })
  } as unknown as ClipboardItem
}

afterEach(() => stubClipboard(undefined))

describe('editor paste images', () => {
  it('recognizes clipboard image files by MIME type or image extension', () => {
    expect(isClipboardImageFile(new File(['x'], 'clip', { type: 'image/png' }))).toBe(true)
    expect(isClipboardImageFile(new File(['x'], 'Screenshot.webp', { type: '' }))).toBe(true)
    expect(isClipboardImageFile(new File(['x'], 'notes.txt', { type: 'text/plain' }))).toBe(false)
  })

  it('converts a pasted image file into a bridge payload', async () => {
    const file = new File([Uint8Array.from([1, 2, 3])], 'clip.png', { type: 'image/png' })

    const input = await pastedImageInputFromFile(file)

    expect(input.mimeType).toBe('image/png')
    expect(input.suggestedName).toBe('clip.png')
    expect([...new Uint8Array(input.data)]).toEqual([1, 2, 3])
  })
})

describe('reading the system clipboard', () => {
  it('returns a clipboard image as a nameless file, with any text beside it', async () => {
    stubClipboard({
      read: vi.fn().mockResolvedValue([clipboardItem({ 'image/png': 'png-bytes', 'text/plain': 'caption' })])
    })

    const { images, text } = await readSystemClipboard()

    expect(images).toHaveLength(1)
    expect(images[0].type).toBe('image/png')
    expect(images[0].name).toBe('')
    expect(text).toBe('caption')
    const input = await pastedImageInputFromFile(images[0])
    expect(input.suggestedName).toBeNull()
  })

  it('reads plain text when the clipboard holds no image', async () => {
    stubClipboard({ read: vi.fn().mockResolvedValue([clipboardItem({ 'text/plain': 'hello' })]) })

    expect(await readSystemClipboard()).toEqual({ images: [], text: 'hello' })
  })

  it('falls back to a text read when a full read is refused', async () => {
    stubClipboard({
      read: vi.fn().mockRejectedValue(new DOMException('denied', 'NotAllowedError')),
      readText: vi.fn().mockResolvedValue('fallback')
    })

    expect(await readSystemClipboard()).toEqual({ images: [], text: 'fallback' })
  })

  it('yields nothing when the clipboard cannot be read at all', async () => {
    stubClipboard({ readText: vi.fn().mockRejectedValue(new Error('blocked')) })
    expect(await readSystemClipboard()).toEqual({ images: [], text: '' })

    stubClipboard(undefined)
    expect(await readSystemClipboard()).toEqual({ images: [], text: '' })
  })
})

describe('pasting clipboard images into an editor', () => {
  const image = new File(['x'], '', { type: 'image/png' })

  it('hands the files to the editor import, at the requested position', () => {
    let headAtImport = -1
    const importImages = vi.fn((_files: File[], view: EditorView) => {
      headAtImport = view.state.selection.main.head
    })
    const view = new EditorView({
      state: EditorState.create({ doc: 'one\ntwo', extensions: [editorImagePaste.of(importImages)] })
    })

    expect(pasteImageFilesIntoEditor(view, [image], 3)).toBe(true)
    expect(importImages).toHaveBeenCalledWith([image], view)
    expect(headAtImport).toBe(3)
    view.destroy()
  })

  it('declines when the editor has no image import or there is no image', () => {
    const importImages = vi.fn()
    const plain = new EditorView({ state: EditorState.create({ doc: 'one' }) })
    const notes = new EditorView({
      state: EditorState.create({ doc: 'one', extensions: [editorImagePaste.of(importImages)] })
    })

    expect(pasteImageFilesIntoEditor(plain, [image], 2)).toBe(false)
    expect(plain.state.selection.main.head).toBe(0)
    expect(pasteImageFilesIntoEditor(notes, [])).toBe(false)
    expect(importImages).not.toHaveBeenCalled()
    plain.destroy()
    notes.destroy()
  })
})
