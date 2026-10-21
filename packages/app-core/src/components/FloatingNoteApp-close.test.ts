// @vitest-environment jsdom

import { act, createElement, type ReactElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { EditorView } from '@codemirror/view'
import { getCM } from '@replit/codemirror-vim'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ExternalFileApp } from './ExternalFileApp'
import { FloatingNoteApp } from './FloatingNoteApp'

const PREFS_KEY = 'zen:prefs:v2'

const windows: Array<[string, () => ReactElement]> = [
  ['a floating note window', () => createElement(FloatingNoteApp, { notePath: 'Inbox/Note.md' })],
  ['a standalone file window', () => createElement(ExternalFileApp)]
]

describe.each(windows)('Close shortcut in %s (#893)', (_, element) => {
  let host: HTMLDivElement
  let root: Root
  let platform: NodeJS.Platform
  const windowClose = vi.fn()

  beforeEach(() => {
    ;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
    vi.clearAllMocks()
    localStorage.clear()
    platform = 'darwin'
    // jsdom has no layout; Vim/CodeMirror still request caret rectangles.
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
        platformSync: () => platform,
        readNote: vi.fn(async () => ({ path: 'Inbox/Note.md', title: 'Note', body: 'Body\n' })),
        writeNote: vi.fn(async () => undefined),
        onVaultChange: vi.fn(() => vi.fn()),
        readExternalFile: vi.fn(async () => ({ path: '/tmp/Note.md', name: 'Note.md', body: 'Body\n' })),
        writeExternalFile: vi.fn(async () => undefined),
        windowClose
      }
    })
    host = document.createElement('div')
    document.body.append(host)
    root = createRoot(host)
  })

  afterEach(() => {
    act(() => root.unmount())
    host.remove()
    localStorage.clear()
  })

  function savePrefs(prefs: Record<string, unknown>): void {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs))
  }

  async function mount(): Promise<EditorView> {
    await act(async () => root.render(element()))
    const view = EditorView.findFromDOM(host.querySelector('.cm-editor')!)!
    act(() => view.focus())
    return view
  }

  async function press(target: EventTarget, init: KeyboardEventInit): Promise<void> {
    await act(async () => {
      target.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init }))
    })
  }

  const cmdW: KeyboardEventInit = { key: 'w', code: 'KeyW', keyCode: 87, metaKey: true }
  const shiftCmdW: KeyboardEventInit = { key: 'W', code: 'KeyW', keyCode: 87, metaKey: true, shiftKey: true }
  const ctrlW: KeyboardEventInit = { key: 'w', code: 'KeyW', keyCode: 87, ctrlKey: true }

  it('closes the window on Cmd+W from the editor, in Vim normal and insert mode', async () => {
    const view = await mount()
    await press(view.contentDOM, cmdW)
    expect(windowClose).toHaveBeenCalledOnce()

    await press(view.contentDOM, { key: 'i', code: 'KeyI', keyCode: 73 })
    expect(getCM(view)?.state.vim?.insertMode).toBe(true)
    await press(view.contentDOM, cmdW)
    expect(windowClose).toHaveBeenCalledTimes(2)
  })

  it('matches by key position on a non-Latin layout', async () => {
    const view = await mount()
    await press(view.contentDOM, { key: 'ц', code: 'KeyW', keyCode: 87, metaKey: true })
    expect(windowClose).toHaveBeenCalledOnce()
  })

  it('follows a rebind of Close active tab', async () => {
    savePrefs({ keymapOverrides: { 'global.closeActiveTab': 'Shift+Mod+W' } })
    const view = await mount()
    await press(view.contentDOM, cmdW)
    expect(windowClose).not.toHaveBeenCalled()
    await press(view.contentDOM, shiftCmdW)
    expect(windowClose).toHaveBeenCalledOnce()
  })

  it('does nothing once Close active tab is unbound', async () => {
    savePrefs({ keymapOverrides: { 'global.closeActiveTab': '' } })
    const view = await mount()
    await press(view.contentDOM, cmdW)
    expect(windowClose).not.toHaveBeenCalled()
  })

  it('picks up a binding changed in the main window while it is open', async () => {
    const view = await mount()
    savePrefs({ keymapOverrides: { 'global.closeActiveTab': 'Shift+Mod+W' } })
    await act(async () => {
      window.dispatchEvent(new StorageEvent('storage', { key: PREFS_KEY }))
    })
    await press(view.contentDOM, cmdW)
    expect(windowClose).not.toHaveBeenCalled()
    await press(view.contentDOM, shiftCmdW)
    expect(windowClose).toHaveBeenCalledOnce()
  })

  it('leaves Ctrl+W alone on macOS, where the shortcut is Cmd+W', async () => {
    savePrefs({ vimMode: false })
    const view = await mount()
    await press(view.contentDOM, ctrlW)
    expect(windowClose).not.toHaveBeenCalled()
  })

  it('keeps the key while a dialog is open over the note', async () => {
    const view = await mount()
    const dialog = document.createElement('div')
    dialog.setAttribute('aria-modal', 'true')
    document.body.append(dialog)
    await press(view.contentDOM, cmdW)
    dialog.remove()
    expect(windowClose).not.toHaveBeenCalled()
  })

  describe('on Linux and Windows (Mod is Ctrl)', () => {
    beforeEach(() => {
      platform = 'linux'
    })

    it('closes the window on Ctrl+W outside the editor', async () => {
      await mount()
      await press(document.body, ctrlW)
      expect(windowClose).toHaveBeenCalledOnce()
    })

    it('leaves Ctrl+W to Vim inside the editor', async () => {
      const view = await mount()
      await press(view.contentDOM, ctrlW)
      expect(windowClose).not.toHaveBeenCalled()
    })
  })
})
