// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing } from '@codemirror/language'
import { EditorState, type Extension } from '@codemirror/state'
import { Decoration, EditorView } from '@codemirror/view'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { NoteMeta } from '@shared/ipc'
import { useStore } from '../store'
import { wikilinkRenderExtension } from './cm-wikilink-render'
import { editorLinkHints, followEditorLinkHint, visibleEditorLinkHints } from './editor-link-hints'
import type { PaneLeaf } from './pane-layout'
import { registerNoteEditor } from './note-editor-context'

const nav = vi.hoisted(() => ({ openWikilinkTarget: vi.fn(async () => true) }))
const create = vi.hoisted(() => ({
  offerCreateNoteFromLink: vi.fn(async () => undefined),
  createNoteFromLinkNow: vi.fn(async () => undefined)
}))

vi.mock('./wikilink-navigation', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./wikilink-navigation')>()),
  openWikilinkTarget: nav.openWikilinkTarget
}))
vi.mock('./create-note-from-link', () => create)

// jsdom has no layout, so each editor is laid out as a grid: line N of an
// editor whose origin is (x, y) starts at top y + 24 * (N - 1), and every
// character is 8px wide from x. CodeMirror measures text through
// Range.getClientRects; characters a decoration replaces are never measured,
// exactly as in a browser.
interface Box {
  left: number
  top: number
  right: number
  bottom: number
}

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

const layouts = new Map<EditorView, { x: number; y: number }>()

function charRects(this: Range): DOMRect[] {
  for (const [view, origin] of layouts) {
    if (!view.contentDOM.contains(this.startContainer)) continue
    let pos: number
    try {
      pos = view.posAtDOM(this.startContainer, this.startOffset)
    } catch {
      return []
    }
    const line = view.state.doc.lineAt(pos)
    return [rect(origin.x + (pos - line.from) * 8, origin.y + (line.number - 1) * 24, 8, 20)]
  }
  return []
}

function mount(
  doc: string,
  options: {
    paneId?: string | null
    path?: string
    extensions?: Extension[]
    origin?: { x: number; y: number }
    box?: Box
  } = {}
): EditorView {
  const parent = document.createElement('div')
  document.body.append(parent)
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc,
      selection: { anchor: 0 },
      extensions: [markdown({ base: markdownLanguage }), ...(options.extensions ?? [])]
    })
  })
  forceParsing(view, doc.length, 5000)
  const paneId = options.paneId === undefined ? 'pane-1' : options.paneId
  if (paneId) registerNoteEditor(view, () => options.path ?? 'Test.md', paneId)
  const origin = options.origin ?? { x: 40, y: 100 }
  const box = options.box ?? {
    left: origin.x - 20,
    top: origin.y - 20,
    right: origin.x + 780,
    bottom: 700
  }
  view.scrollDOM.getBoundingClientRect = () =>
    rect(box.left, box.top, box.right - box.left, box.bottom - box.top)
  layouts.set(view, origin)
  return view
}

/** Hide [from, to) the way live preview hides syntax and embedded sources. */
function hidden(from: number, to: number): Extension {
  return EditorView.decorations.of(Decoration.set([Decoration.replace({}).range(from, to)]))
}

function note(path: string): NoteMeta {
  const title = path.split('/').pop()!.replace(/\.md$/, '')
  return {
    path,
    title,
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
}

function leaf(id: string, path: string): PaneLeaf {
  return { kind: 'leaf', id, tabs: [path], pinnedTabs: [], activeTab: path }
}

const initialStore = useStore.getState()

beforeEach(() => {
  ;(Range.prototype as unknown as { getClientRects: unknown }).getClientRects = charRects
  ;(Range.prototype as unknown as { getBoundingClientRect: unknown }).getBoundingClientRect =
    function (this: Range) {
      return charRects.call(this)[0] ?? rect(0, 0, 0, 0)
    }
})

afterEach(() => {
  for (const view of layouts.keys()) {
    view.dom.parentElement?.remove()
    view.destroy()
  }
  layouts.clear()
  delete (Range.prototype as unknown as { getClientRects?: unknown }).getClientRects
  delete (Range.prototype as unknown as { getBoundingClientRect?: unknown }).getBoundingClientRect
  useStore.setState(initialStore, true)
  vi.restoreAllMocks()
  nav.openWikilinkTarget.mockClear()
  create.offerCreateNoteFromLink.mockClear()
  create.createNoteFromLinkNow.mockClear()
})

const targets = (view: EditorView): string[] =>
  editorLinkHints(view, 'pane-1').map((hint) => hint.target)

describe('editorLinkHints (#894)', () => {
  it('finds bare URLs, wikilinks with an alias or a heading, and Markdown links and images', () => {
    const view = mount(
      [
        '# Links',
        'https://example.net/x',
        '[[Plan|the plan]] then [[Plan#Goals]]',
        '![diagram](assets/d.png) and [spec](<docs/the spec.md>)'
      ].join('\n')
    )

    const hints = editorLinkHints(view, 'pane-1')

    expect(hints.map(({ kind, target, paneId }) => ({ kind, target, paneId }))).toEqual([
      { kind: 'url', target: 'https://example.net/x', paneId: 'pane-1' },
      { kind: 'wikilink', target: 'Plan', paneId: 'pane-1' },
      { kind: 'wikilink', target: 'Plan#Goals', paneId: 'pane-1' },
      { kind: 'markdown', target: 'assets/d.png', paneId: 'pane-1' },
      { kind: 'markdown', target: 'docs/the spec.md', paneId: 'pane-1' }
    ])
    // The URL's box runs from its first character to its last on that row.
    expect(hints[0].rect).toEqual({ left: 40, top: 124, right: 40 + 21 * 8, bottom: 144 })
  })

  it('leaves links in code spans and fenced code alone, as the renderer does', () => {
    const view = mount(
      [
        '`[[Not a link]]` and `https://code.test`',
        '```text',
        'https://fenced.test [[Fenced]]',
        '```',
        '[[Real]]'
      ].join('\n')
    )

    expect(targets(view)).toEqual(['Real'])
  })

  it('labels the first character live preview draws, not the hidden syntax', () => {
    const doc = 'see [[Plan|the plan]]'
    const view = mount(doc, { extensions: [wikilinkRenderExtension] })

    const [hint] = editorLinkHints(view, 'pane-1')

    expect(hint.target).toBe('Plan')
    expect(hint.rect.left).toBe(40 + doc.indexOf('the plan') * 8)
    expect(hint.anchor.parentElement?.closest('.cm-wikilink')).not.toBeNull()
  })

  it('gives no hint to a link whose whole source is hidden, as behind an image embed', () => {
    const image = '![diagram](assets/d.png)'
    const view = mount(`${image}\n[[Plan]]`, { extensions: [hidden(0, image.length)] })

    expect(targets(view)).toEqual(['Plan'])
  })

  it('counts only links drawn inside the visible part of the editor', () => {
    // Line 4 sits at 172..192, inside the box; lines 1 and 8 are drawn (as
    // CodeMirror draws past the scroller's edges) but outside it. `[[Far]]`
    // starts past the box's right edge.
    const view = mount(
      ['[[One]]', '', '', `[[Four]]${' '.repeat(40)}[[Far]]`, '', '', '', '[[Eight]]'].join('\n'),
      { box: { left: 20, top: 160, right: 300, bottom: 240 } }
    )

    expect(targets(view)).toEqual(['Four'])
  })

  it('has nothing to label in an editor that is not on screen', () => {
    // A pane in Preview mode keeps its editor mounted, with display: none.
    const view = mount('[[Plan]]', { box: { left: 0, top: 0, right: 0, bottom: 0 } })

    expect(targets(view)).toEqual([])
  })

  it('skips a blank wikilink and trims a target, as the rendered link does', () => {
    const view = mount('[[ ]] and [[ Spaced ]]')

    expect(targets(view)).toEqual(['Spaced'])
  })
})

describe('visibleEditorLinkHints (#894)', () => {
  it('reads every note editor on the page, and no other editor', () => {
    mount('[[Left]]', { paneId: 'pane-1' })
    mount('[[Right]]', { paneId: 'pane-2', origin: { x: 440, y: 100 } })
    // Any other CodeMirror (a template editor, the reference pane) is no
    // note editor, and its links do not follow from a pane.
    mount('[[Other]]', { paneId: null, origin: { x: 40, y: 400 } })

    expect(visibleEditorLinkHints().map((hint) => [hint.paneId, hint.target])).toEqual([
      ['pane-1', 'Left'],
      ['pane-2', 'Right']
    ])
  })
})

describe('followEditorLinkHint (#894)', () => {
  beforeEach(() => {
    useStore.setState({
      notes: [
        note('inbox/Other.md'),
        note('inbox/plan.md'),
        note('projects/Spec.md'),
        note('projects/plan.md'),
        note('daily/2024.01.15.md')
      ],
      paneLayout: {
        kind: 'split',
        id: 'root',
        direction: 'row',
        sizes: [50, 50],
        children: [leaf('pane-1', 'inbox/Other.md'), leaf('pane-2', 'projects/Spec.md')]
      },
      activePaneId: 'pane-1',
      selectedPath: 'inbox/Other.md',
      focusedPanel: 'sidebar',
      selectNote: vi.fn(async () => undefined)
    })
  })

  function hintIn(doc: string): ReturnType<typeof editorLinkHints>[number] {
    const view = mount(doc, { paneId: 'pane-2', path: 'projects/Spec.md' })
    const [hint] = editorLinkHints(view, 'pane-2')
    return hint
  }

  it('makes the link’s pane active first, so a relative link resolves against its note', () => {
    followEditorLinkHint(hintIn('[the plan](plan.md)'))

    expect(useStore.getState().activePaneId).toBe('pane-2')
    expect(useStore.getState().selectNote).toHaveBeenCalledWith('projects/plan.md')
  })

  it('opens the note a wikilink names, even when the name reads like a domain', () => {
    const open = vi.spyOn(window, 'open').mockImplementation(() => null)

    followEditorLinkHint(hintIn('[[2024.01.15]]'))

    expect(nav.openWikilinkTarget).toHaveBeenCalledWith('daily/2024.01.15.md', '2024.01.15')
    expect(open).not.toHaveBeenCalled()
  })

  it('asks before creating the note a dead wikilink names, like a plain click', () => {
    followEditorLinkHint(hintIn('[[Missing]]'))

    expect(create.offerCreateNoteFromLink).toHaveBeenCalledWith('Missing')
    expect(create.createNoteFromLinkNow).not.toHaveBeenCalled()
  })

  it('sends a bare URL to the browser and leaves the keyboard in that editor', () => {
    const open = vi.spyOn(window, 'open').mockImplementation(() => null)
    const hint = hintIn('https://example.net')

    followEditorLinkHint(hint)

    expect(open).toHaveBeenCalledWith('https://example.net', '_blank')
    expect(useStore.getState().focusedPanel).toBe('editor')
    expect(hint.view.hasFocus).toBe(true)
  })
})
