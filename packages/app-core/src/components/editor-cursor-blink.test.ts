// @vitest-environment jsdom
//
// Guards #892: Blinking cursor reached only the main editor. The floating
// note, Quick Capture and external-file windows build their own CodeMirror
// from the stored prefs blob, and they, the reference pane and the template
// editor all built a bare drawSelection(), which blinks at CodeMirror's
// default whatever the preference says.

import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { act, createElement, type ReactElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { NoteContent } from '@shared/ipc'
import { useStore } from '../store'
import { ExternalFileApp } from './ExternalFileApp'
import { FloatingNoteApp } from './FloatingNoteApp'
import { PinnedReferencePane } from './PinnedReferencePane'
import { QuickCaptureApp } from './QuickCaptureApp'
import { TemplateEditorModal } from './TemplateEditorModal'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const PREFS_KEY = 'zen:prefs:v2'

const note: NoteContent = {
  path: 'inbox/Note.md',
  title: 'Note',
  folder: 'inbox',
  siblingOrder: 0,
  createdAt: 0,
  updatedAt: 0,
  size: 10,
  tags: [],
  wikilinks: [],
  assetEmbeds: [],
  hasAttachments: false,
  excerpt: 'alpha beta',
  body: 'alpha beta'
}

/** How the mounted editor's cursors blink. CodeMirror's caret layer and the
 *  Vim block cursor layer both copy drawSelection's cursorBlinkRate into an
 *  inline animation-duration, and 0ms never blinks. `block` is null with Vim
 *  off. */
function cursorBlink(): { caret: string | null; block: string | null } {
  const editor = document.querySelector('.cm-editor')
  if (!editor) throw new Error('no editor mounted')
  const duration = (selector: string): string | null =>
    editor.querySelector<HTMLElement>(selector)?.style.animationDuration ?? null
  return { caret: duration('.cm-layer.cm-cursorLayer'), block: duration('.cm-vimCursorLayer') }
}

const SOLID = { caret: '0ms', block: '0ms' }
const BLINKING = { caret: '1200ms', block: '1200ms' }

let host: HTMLDivElement
let root: Root
let originalState: ReturnType<typeof useStore.getState>

beforeEach(() => {
  localStorage.clear()
  originalState = useStore.getState()
  // jsdom has no layout; CodeMirror and Vim still measure the caret.
  Object.defineProperty(Range.prototype, 'getClientRects', {
    configurable: true,
    value: () => []
  })
  Object.defineProperty(Range.prototype, 'getBoundingClientRect', {
    configurable: true,
    value: () => new DOMRect()
  })
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }))
  })
  Object.defineProperty(window, 'zen', {
    configurable: true,
    value: {
      readNote: vi.fn(async () => note),
      readExternalFile: vi.fn(async () => ({
        path: '/outside/Loose.md',
        name: 'Loose.md',
        body: 'alpha beta'
      })),
      listNotes: vi.fn(async () => []),
      onVaultChange: vi.fn(() => vi.fn()),
      getQuickCapturePinned: vi.fn(async () => false),
      platformSync: () => 'linux',
      writeNote: vi.fn(async () => undefined),
      writeExternalFile: vi.fn(async () => undefined),
      windowClose: vi.fn()
    }
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  useStore.setState(originalState, true)
  localStorage.clear()
})

async function render(element: ReactElement): Promise<void> {
  await act(async () => root.render(element))
}

describe.each([
  ['floating note window', () => createElement(FloatingNoteApp, { notePath: note.path })],
  ['Quick Capture window', () => createElement(QuickCaptureApp)],
  ['external file window', () => createElement(ExternalFileApp)]
] as const)('the %s', (_, app) => {
  async function open(prefs?: Record<string, unknown>): Promise<void> {
    if (prefs) localStorage.setItem(PREFS_KEY, JSON.stringify(prefs))
    await render(app())
  }

  /** Settings saves the prefs blob in the main window; another window hears
   *  of it only as a `storage` event. */
  async function saveInMainWindow(prefs: Record<string, unknown>): Promise<void> {
    const newValue = JSON.stringify(prefs)
    localStorage.setItem(PREFS_KEY, newValue)
    await act(async () => {
      window.dispatchEvent(
        new StorageEvent('storage', { key: PREFS_KEY, newValue, storageArea: localStorage })
      )
    })
  }

  it('keeps the caret and the Vim block cursor solid when blinking is off', async () => {
    await open({ cursorBlink: false })
    expect(cursorBlink()).toEqual(SOLID)
  })

  it('keeps the caret solid with Vim off', async () => {
    await open({ cursorBlink: false, vimMode: false })
    expect(cursorBlink()).toEqual({ caret: '0ms', block: null })
  })

  it('still blinks by default', async () => {
    await open()
    expect(cursorBlink()).toEqual(BLINKING)
  })

  it('follows the preference when it changes while the window is open', async () => {
    await open({ cursorBlink: true })
    await saveInMainWindow({ cursorBlink: false })
    expect(cursorBlink()).toEqual(SOLID)
    await saveInMainWindow({ cursorBlink: true })
    expect(cursorBlink()).toEqual(BLINKING)
  })
})

describe('main-window editors', () => {
  it('keeps the reference pane cursor solid and follows the preference', async () => {
    useStore.setState({
      vault: { root: '/vault', name: 'Vault' },
      selectedPath: note.path,
      pinnedRefPath: note.path,
      pinnedRefKind: 'note',
      pinnedRefVisible: true,
      pinnedRefMode: 'edit',
      noteRefs: {},
      noteContents: { [note.path]: note },
      noteDirty: {},
      vimMode: true,
      cursorBlink: false
    })
    await render(createElement(PinnedReferencePane))
    expect(cursorBlink()).toEqual(SOLID)

    await act(async () => useStore.setState({ cursorBlink: true }))
    expect(cursorBlink()).toEqual(BLINKING)
  })

  it('keeps the template editor cursor solid', async () => {
    useStore.setState({ vimMode: true, cursorBlink: false })
    await render(createElement(TemplateEditorModal, { onClose: vi.fn() }))
    expect(cursorBlink()).toEqual(SOLID)
  })

  it('builds drawSelection only through the cursor blink helper', () => {
    // A bare drawSelection() blinks no matter the preference, which is how
    // five editors came to ignore it.
    const src = join(dirname(fileURLToPath(import.meta.url)), '..')
    const sources = readdirSync(src, { recursive: true, encoding: 'utf8' }).filter(
      (file) => /\.tsx?$/.test(file) && !/\.test\.tsx?$/.test(file)
    )
    // A scan that stopped finding the sources would pass on nothing.
    expect(sources.length).toBeGreaterThan(100)
    const bare = sources.filter(
      (file) =>
        file !== join('lib', 'cm-cursor-blink.ts') &&
        /\bdrawSelection\(/.test(readFileSync(join(src, file), 'utf8'))
    )
    expect(bare).toEqual([])
  })
})
