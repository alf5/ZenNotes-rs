// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing, syntaxTreeAvailable } from '@codemirror/language'
import { Compartment, EditorState, type EditorSelection, type Extension } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { noteMarkdown } from './cm-markdown-language'
import { mermaidBlockLineRanges, mermaidRenderExtension } from './cm-mermaid-render'

// mermaid itself is never loaded here: the widget paints asynchronously and
// these tests are about WHICH blocks become widgets and where their lines are,
// which is decided before any rendering happens. Stubbing keeps the heaviest
// chunk in the app out of the test run entirely.
const mermaidMocks = vi.hoisted(() => ({
  peekMermaidSvg: vi.fn(),
  renderMermaidSvg: vi.fn()
}))

vi.mock('./mermaid-render', () => mermaidMocks)

function mount(
  doc: string,
  selection?: EditorSelection | { anchor: number },
  extension: Extension = mermaidRenderExtension('light', 'theme-a')
): EditorView {
  const parent = document.createElement('div')
  document.body.append(parent)
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc,
      selection: selection ?? { anchor: 0 },
      extensions: [markdown({ base: markdownLanguage }), extension]
    })
  })
  // The field rebuilds whenever the syntax tree changes, so finishing the parse
  // is all a mount needs. Nothing edits the note after it: a note this short
  // parses from scratch on every edit, under the same 20 ms budget, and a cut
  // parse drops the fence from the tree, so the block comes back as a new
  // widget (a second render) or not at all.
  forceParsing(view, doc.length, 5000)
  return view
}

const DIAGRAM = 'start\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\nend'

/** Runs `edit` while every clock read jumps 25 ms, so the parse that follows
 *  runs out of CodeMirror's 20 ms budget after its first step, as it does on a
 *  stalled worker. Here that step parses `start` and stops before the fence. */
function withStalledClock(edit: () => void): void {
  let now = Date.now()
  const clock = vi.spyOn(Date, 'now').mockImplementation(() => (now += 25))
  try {
    edit()
  } finally {
    clock.mockRestore()
  }
}

/** Lets a mocked render's promise settle. */
const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0))

describe('mermaidRenderExtension', () => {
  beforeEach(() => {
    mermaidMocks.peekMermaidSvg.mockReset().mockReturnValue(null)
    mermaidMocks.renderMermaidSvg.mockReset().mockImplementation(() => new Promise(() => {}))
  })

  it('draws a mermaid fence while the cursor is elsewhere', () => {
    const view = mount(DIAGRAM)
    expect(view.dom.querySelectorAll('.cm-mermaid-block').length).toBe(1)
    view.destroy()
  })

  it('reveals the source while the cursor is inside the block', () => {
    // Anchor on the `flowchart LR` line, i.e. inside the fence.
    const view = mount(DIAGRAM, { anchor: DIAGRAM.indexOf('flowchart') + 2 })
    expect(view.dom.querySelectorAll('.cm-mermaid-block').length).toBe(0)
    view.destroy()
  })

  it('leaves other languages alone', () => {
    const view = mount('start\n\n```ts\nconst a = 1\n```\n\nend')
    expect(view.dom.querySelectorAll('.cm-mermaid-block').length).toBe(0)
    view.destroy()
  })

  it('accepts an info string with extra words after the language', () => {
    const view = mount('start\n\n```mermaid title="Flow"\nflowchart LR\n  A --> B\n```\n\nend')
    expect(view.dom.querySelectorAll('.cm-mermaid-block').length).toBe(1)
    view.destroy()
  })

  it('leaves an empty fence as source, since there is nothing to draw', () => {
    const view = mount('start\n\n```mermaid\n```\n\nend')
    expect(view.dom.querySelectorAll('.cm-mermaid-block').length).toBe(0)
    view.destroy()
  })

  // The theme identity carries the text font, and a new font has to be measured
  // again, so the widget is rebuilt and asks the renderer (which answers from
  // its cache when only the palette changed).
  it('rebuilds when the theme identity changes without changing light/dark mode', () => {
    mermaidMocks.renderMermaidSvg.mockResolvedValue({ ok: true, svg: '<svg data-theme="a" />' })
    const theme = new Compartment()
    const view = mount(DIAGRAM, undefined, theme.of(mermaidRenderExtension('light', 'theme-a')))
    const source = 'flowchart LR\n  A --> B'
    expect(mermaidMocks.renderMermaidSvg.mock.calls).toEqual([[source]])

    view.dispatch({
      effects: theme.reconfigure(mermaidRenderExtension('light', 'theme-b'))
    })

    expect(mermaidMocks.renderMermaidSvg.mock.calls).toEqual([[source], [source]])
    view.destroy()
  })

  it('keeps the last good drawing for a block while its edited source is invalid', async () => {
    mermaidMocks.renderMermaidSvg.mockImplementation((source: string) =>
      Promise.resolve(
        source.includes('INVALID')
          ? { ok: false, error: 'parse failed' }
          : { ok: true, svg: '<svg data-last-good="true" />' }
      )
    )
    const view = mount(DIAGRAM)

    await vi.waitFor(() =>
      expect(view.dom.querySelector('[data-last-good="true"]')).not.toBeNull()
    )
    const from = view.state.doc.toString().indexOf('flowchart LR')
    view.dispatch({ changes: { from, to: from + 'flowchart LR'.length, insert: 'INVALID' } })

    await vi.waitFor(() => {
      expect(view.dom.querySelector('[data-last-good="true"]')).not.toBeNull()
      expect(view.dom.querySelector('.cm-mermaid-error')).toBeNull()
    })
    view.destroy()
  })

  // A parse that runs out of time keeps only the blocks it reached, and the
  // rest of the note catches up on a later update. The block is still there in
  // between, so it must still have its last good drawing when the parse comes
  // back to it. The app's note language pads a cut tree to the note's length,
  // which is why both languages are checked.
  describe('when a parse runs out of time before the block', () => {
    function mountIn(language: Extension, doc = DIAGRAM): EditorView {
      const parent = document.createElement('div')
      document.body.append(parent)
      const view = new EditorView({
        parent,
        state: EditorState.create({
          doc,
          selection: { anchor: 0 },
          extensions: [language, mermaidRenderExtension('light', 'theme-a')]
        })
      })
      forceParsing(view, doc.length, 5000)
      return view
    }

    beforeEach(() => {
      mermaidMocks.renderMermaidSvg.mockImplementation((source: string) =>
        Promise.resolve(
          source.includes('INVALID')
            ? { ok: false, error: 'parse failed' }
            : { ok: true, svg: '<svg data-last-good="true" />' }
        )
      )
    })

    it.each([
      ['markdown', () => markdown({ base: markdownLanguage })],
      ['the note language', () => noteMarkdown()]
    ])('keeps the last good drawing through the cut parse (%s)', async (_name, language) => {
      const view = mountIn(language())
      await vi.waitFor(() =>
        expect(view.dom.querySelector('[data-last-good="true"]')).not.toBeNull()
      )
      const from = DIAGRAM.indexOf('flowchart LR')
      withStalledClock(() =>
        view.dispatch({ changes: { from, to: from + 'flowchart LR'.length, insert: 'INVALID' } })
      )
      // The edit's parse stopped before the fence, so the block is out of the tree.
      expect(syntaxTreeAvailable(view.state)).toBe(false)
      expect(mermaidBlockLineRanges(view.state)).toEqual([])

      forceParsing(view, view.state.doc.length, 5000)
      expect(mermaidMocks.renderMermaidSvg).toHaveBeenLastCalledWith('INVALID\n  A --> B')
      await settle()
      expect(view.dom.querySelector('[data-last-good="true"]')).not.toBeNull()
      expect(view.dom.querySelector('.cm-mermaid-error')).toBeNull()
      view.destroy()
    })

    it('still forgets a block once a complete parse no longer has it', async () => {
      // The fence ends the note, so nothing the parse reaches comes after it.
      const doc = 'start\n\n```mermaid\nflowchart LR\n  A --> B\n```'
      const view = mountIn(markdown({ base: markdownLanguage }), doc)
      await vi.waitFor(() =>
        expect(view.dom.querySelector('[data-last-good="true"]')).not.toBeNull()
      )
      const from = doc.indexOf('```mermaid')
      withStalledClock(() => view.dispatch({ changes: { from, to: doc.length } }))
      expect(syntaxTreeAvailable(view.state)).toBe(false)
      forceParsing(view, view.state.doc.length, 5000)

      // A new fence typed where the old one was is a new block: with an invalid
      // source it has no drawing to fall back on.
      view.dispatch({ changes: { from, insert: '```mermaid\nINVALID\n```' } })
      forceParsing(view, view.state.doc.length, 5000)
      expect(mermaidMocks.renderMermaidSvg).toHaveBeenLastCalledWith('INVALID')
      await settle()
      expect(view.dom.querySelector('.cm-mermaid-error')).not.toBeNull()
      expect(view.dom.querySelector('[data-last-good="true"]')).toBeNull()
      view.destroy()
    })
  })

  // A rendered block is one widget with no cursor coordinates inside it, so
  // vertical motion sails over it unless the nav helpers know the block is
  // there. Without these ranges a keyboard-only user can never open the source.
  describe('mermaidBlockLineRanges', () => {
    it('reports the fence line span, fences included', () => {
      const view = mount(DIAGRAM)
      expect(mermaidBlockLineRanges(view.state)).toEqual([{ fromLine: 3, toLine: 6 }])
      view.destroy()
    })

    it('still reports the block while its source is revealed', () => {
      const view = mount(DIAGRAM, { anchor: DIAGRAM.indexOf('flowchart') + 2 })
      expect(mermaidBlockLineRanges(view.state)).toEqual([{ fromLine: 3, toLine: 6 }])
      view.destroy()
    })

    it('is empty when the extension is not installed', () => {
      const parent = document.createElement('div')
      document.body.append(parent)
      const plain = new EditorView({
        parent,
        state: EditorState.create({
          doc: DIAGRAM,
          extensions: [markdown({ base: markdownLanguage })]
        })
      })
      expect(mermaidBlockLineRanges(plain.state)).toEqual([])
      plain.destroy()
    })
  })
})
