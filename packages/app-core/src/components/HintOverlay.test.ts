// @vitest-environment jsdom

import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { NoteMeta } from '@shared/ipc'
import { useStore } from '../store'
import { wikilinkRenderExtension } from '../lib/cm-wikilink-render'
import { registerNoteEditor } from '../lib/note-editor-context'
import { HintOverlay } from './HintOverlay'

const nav = vi.hoisted(() => ({ openWikilinkTarget: vi.fn(async () => true) }))

vi.mock('../lib/wikilink-navigation', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../lib/wikilink-navigation')>()),
  openWikilinkTarget: nav.openWikilinkTarget
}))
;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

// The note from the report, shown in the Edit view.
const NOTE = '# Test\n\nhttps://example.net\n\n[[ZenNotes]]\n\n[GitHub](https://github.com)'

const zenNotes = {
  path: 'ZenNotes.md',
  title: 'ZenNotes',
  folder: 'inbox',
  siblingOrder: 0,
  createdAt: 0,
  updatedAt: 0,
  size: 0,
  tags: [],
  wikilinks: [],
  assetEmbeds: [],
  hasAttachments: false,
  excerpt: ''
} as NoteMeta

// jsdom has no layout. Lay the editor out as a grid instead: line N starts at
// top 100 + 24 * (N - 1), every character is 8px wide from left 40, and the
// editor's scroller is a 800x620 box at (20, 80). CodeMirror measures text
// through Range.getClientRects, so that is the one primitive to provide;
// characters a decoration replaces are never measured, as in a browser.
function rect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    x: left,
    y: top,
    toJSON: () => ({})
  } as DOMRect
}

const SCROLLER = rect(20, 80, 800, 620)

let layoutView: EditorView | null = null

function charRects(this: Range): DOMRect[] {
  const view = layoutView
  if (!view || !view.contentDOM.contains(this.startContainer)) return []
  let pos: number
  try {
    pos = view.posAtDOM(this.startContainer, this.startOffset)
  } catch {
    return []
  }
  const line = view.state.doc.lineAt(pos)
  return [rect(40 + (pos - line.from) * 8, 100 + (line.number - 1) * 24, 8, 20)]
}

function mountNoteEditor(doc: string, paneId: string): EditorView {
  const parent = document.createElement('div')
  document.body.append(parent)
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc,
      selection: { anchor: 0 },
      extensions: [markdown({ base: markdownLanguage }), wikilinkRenderExtension]
    })
  })
  registerNoteEditor(view, () => 'Test.md', paneId)
  view.scrollDOM.getBoundingClientRect = () => SCROLLER
  layoutView = view
  return view
}

const initialStore = useStore.getState()

describe('HintOverlay over the Edit view (#894)', () => {
  let view: EditorView
  let host: HTMLDivElement
  let root: Root
  const openSpy = vi.fn(() => null)

  beforeEach(() => {
    nav.openWikilinkTarget.mockClear()
    openSpy.mockClear()
    vi.spyOn(window, 'open').mockImplementation(openSpy)
    ;(Range.prototype as unknown as { getClientRects: unknown }).getClientRects = charRects
    ;(Range.prototype as unknown as { getBoundingClientRect: unknown }).getBoundingClientRect =
      function (this: Range) {
        return charRects.call(this)[0] ?? rect(0, 0, 0, 0)
      }
    useStore.setState({
      notes: [zenNotes],
      selectedPath: 'Test.md',
      activePaneId: 'pane-1',
      paneLayout: {
        kind: 'leaf',
        id: 'pane-1',
        tabs: ['Test.md'],
        pinnedTabs: [],
        activeTab: 'Test.md'
      }
    })
    view = mountNoteEditor(NOTE, 'pane-1')
    host = document.createElement('div')
    document.body.append(host)
    root = createRoot(host)
  })

  afterEach(() => {
    act(() => root.unmount())
    host.remove()
    for (const control of document.querySelectorAll('[data-test-control]')) control.remove()
    view.dom.parentElement?.remove()
    view.destroy()
    layoutView = null
    delete (Range.prototype as unknown as { getClientRects?: unknown }).getClientRects
    delete (Range.prototype as unknown as { getBoundingClientRect?: unknown }).getBoundingClientRect
    useStore.setState(initialStore, true)
    vi.restoreAllMocks()
  })

  async function showHints(): Promise<{
    onActivate: ReturnType<typeof vi.fn>
    onCancel: ReturnType<typeof vi.fn>
  }> {
    const onActivate = vi.fn()
    const onCancel = vi.fn()
    await act(async () => {
      root.render(createElement(HintOverlay, { onActivate, onCancel }))
    })
    return { onActivate, onCancel }
  }

  async function press(key: string): Promise<void> {
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }))
    })
    // A unique match fires after a short beat so the label can be seen.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 80))
    })
  }

  function hints(): Array<{ label: string; left: string; top: string }> {
    return Array.from(document.querySelectorAll<HTMLElement>('.vim-hint')).map((el) => ({
      label: el.textContent ?? '',
      left: el.style.left,
      top: el.style.top
    }))
  }

  it('labels each link the editor shows, at its first visible character', async () => {
    const { onCancel } = await showHints()

    expect(onCancel).not.toHaveBeenCalled()
    expect(hints()).toEqual([
      // https://example.net, line 3 from its first character
      { label: 'a', left: '44px', top: '152px' },
      // [[ZenNotes]], line 5: the brackets are hidden, so the label sits on the Z
      { label: 's', left: '60px', top: '200px' },
      // [GitHub](https://github.com), line 7; its URL is part of it, not a hint of its own
      { label: 'd', left: '44px', top: '248px' }
    ])
  })

  it('follows a bare URL out to the browser', async () => {
    const { onActivate } = await showHints()
    await press('a')

    expect(openSpy).toHaveBeenCalledWith('https://example.net', '_blank')
    expect(onActivate).toHaveBeenCalledTimes(1)
  })

  it('opens the note a wikilink names, like clicking it', async () => {
    const { onActivate } = await showHints()
    await press('s')

    expect(nav.openWikilinkTarget).toHaveBeenCalledWith('ZenNotes.md', 'ZenNotes')
    expect(openSpy).not.toHaveBeenCalled()
    expect(onActivate).toHaveBeenCalledTimes(1)
  })

  it('follows a Markdown link to its URL', async () => {
    await showHints()
    await press('d')

    expect(openSpy).toHaveBeenCalledWith('https://github.com', '_blank')
  })

  it('hands out labels in document order, the links among the page’s controls', async () => {
    const control = (name: string, top: number): HTMLButtonElement => {
      const button = document.createElement('button')
      button.textContent = name
      button.dataset.testControl = ''
      button.getBoundingClientRect = () => rect(0, top, 100, 20)
      return button
    }
    const sidebarRow = control('Sidebar row', 0)
    const statusButton = control('Status bar', 740)
    document.body.prepend(sidebarRow)
    document.body.append(statusButton)
    const clicked = vi.fn()
    sidebarRow.addEventListener('click', clicked)

    const { onActivate } = await showHints()

    expect(hints().map((hint) => hint.label)).toEqual(['a', 's', 'd', 'f', 'g'])
    expect(hints()[0]).toEqual({ label: 'a', left: '4px', top: '4px' })
    expect(hints()[4]).toEqual({ label: 'g', left: '4px', top: '744px' })

    // A control still activates as before: clicked, focused, handed back.
    await press('a')
    expect(clicked).toHaveBeenCalledTimes(1)
    expect(document.activeElement).toBe(sidebarRow)
    expect(onActivate).toHaveBeenCalledWith(sidebarRow)
    expect(openSpy).not.toHaveBeenCalled()
  })
})
