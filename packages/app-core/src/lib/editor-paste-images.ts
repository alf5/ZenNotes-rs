import { Facet } from '@codemirror/state'
import type { EditorView } from '@codemirror/view'
import type { PastedImageInput } from '@shared/ipc'

const IMAGE_FILE_EXTENSION_RE = /\.(apng|avif|gif|jpe?g|png|svg|webp)$/i

export function isClipboardImageFile(file: File): boolean {
  if (file.type.toLowerCase().startsWith('image/')) return true
  return IMAGE_FILE_EXTENSION_RE.test(file.name)
}

export function pastedImageFilesFromClipboard(dataTransfer: DataTransfer | null): File[] {
  if (!dataTransfer) return []
  const direct = Array.from(dataTransfer.files ?? []).filter(isClipboardImageFile)
  if (direct.length > 0) return direct

  return Array.from(dataTransfer.items ?? [])
    .filter((item) => item.kind === 'file')
    .map((item) => item.getAsFile())
    .filter((file): file is File => !!file && isClipboardImageFile(file))
}

export async function pastedImageInputFromFile(file: File): Promise<PastedImageInput> {
  return {
    data: await file.arrayBuffer(),
    mimeType: file.type || 'image/png',
    suggestedName: file.name || null
  }
}

export interface SystemClipboardContents {
  images: File[]
  text: string
}

/**
 * Reads the system clipboard for the paste commands that never get a paste
 * event: the editor menu's Paste and Vim's `+` and `*` registers. An image comes
 * back as a nameless file, so the vault names it the way it names a pasted
 * screenshot. A read the platform refuses yields nothing rather than throwing.
 */
export async function readSystemClipboard(): Promise<SystemClipboardContents> {
  const clipboard = typeof navigator === 'undefined' ? undefined : navigator.clipboard
  if (clipboard?.read) {
    try {
      const images: File[] = []
      let text = ''
      for (const item of await clipboard.read()) {
        const imageType = item.types.find((type) => type.toLowerCase().startsWith('image/'))
        if (imageType) images.push(new File([await item.getType(imageType)], '', { type: imageType }))
        if (!text && item.types.includes('text/plain')) {
          text = await (await item.getType('text/plain')).text()
        }
      }
      return { images, text }
    } catch {
      // Some platforms refuse a full read but still allow a text read.
    }
  }
  try {
    return { images: [], text: (await clipboard?.readText()) ?? '' }
  } catch {
    return { images: [], text: '' }
  }
}

/** Imports image files into the note an editor shows, the way Mod+V does. */
export type EditorImagePaste = (files: File[], view: EditorView) => void

/**
 * The note editor's image import, for paste commands that get no paste event.
 * Editors without it (table cells) leave clipboard images to the text fallback.
 */
export const editorImagePaste = Facet.define<EditorImagePaste, EditorImagePaste | null>({
  combine: (handlers) => handlers[0] ?? null
})

/**
 * Pastes clipboard images into `view` through its note import, at `at` when
 * given, else at the cursor. False when the editor cannot take images.
 */
export function pasteImageFilesIntoEditor(view: EditorView, files: File[], at?: number): boolean {
  const importImages = view.state.facet(editorImagePaste)
  if (!importImages || files.length === 0) return false
  if (at !== undefined) view.dispatch({ selection: { anchor: at } })
  importImages(files, view)
  return true
}
