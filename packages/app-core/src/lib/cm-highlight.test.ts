// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing } from '@codemirror/language'
import { Compartment, EditorSelection, EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { afterEach, describe, expect, it } from 'vitest'
import {
  applyHighlight,
  HIGHLIGHT_COLORS,
  highlightExtension,
  unwrapHighlight,
  wrapHighlight
} from './cm-highlight'

const views: EditorView[] = []
function mount(doc: string, from: number, to: number): EditorView {
  const parent = document.createElement('div')
  document.body.append(parent)
  const view = new EditorView({
    parent,
    state: EditorState.create({ doc, selection: EditorSelection.range(from, to) })
  })
  views.push(view)
  return view
}
afterEach(() => {
  while (views.length) views.pop()!.destroy()
})

describe('wrapHighlight', () => {
  it('uses == for yellow (default) and <mark class> for colors', () => {
    expect(wrapHighlight('x', 'yellow')).toBe('==x==')
    expect(wrapHighlight('foo bar', 'green')).toBe('<mark class="hl-green">foo bar</mark>')
    expect(wrapHighlight('x', 'blue')).toBe('<mark class="hl-blue">x</mark>')
    expect(wrapHighlight('x', 'purple')).toBe('<mark class="hl-purple">x</mark>')
    expect(wrapHighlight('x', 'red')).toBe('<mark class="hl-red">x</mark>')
  })
})

describe('unwrapHighlight', () => {
  it('strips == and <mark> wrappers', () => {
    expect(unwrapHighlight('==x==')).toBe('x')
    expect(unwrapHighlight('<mark class="hl-green">x</mark>')).toBe('x')
    expect(unwrapHighlight('<mark>x</mark>')).toBe('x')
  })

  it('leaves plain (or partial) text untouched', () => {
    expect(unwrapHighlight('plain')).toBe('plain')
    expect(unwrapHighlight('==')).toBe('==')
    expect(unwrapHighlight('a == b')).toBe('a == b')
  })

  it('round-trips with wrapHighlight for every color (enables re-coloring)', () => {
    for (const c of HIGHLIGHT_COLORS) {
      expect(unwrapHighlight(wrapHighlight('hello world', c.id))).toBe('hello world')
    }
  })
})

describe('applyHighlight (#416)', () => {
  it('highlights the live selection', () => {
    const view = mount('the word here', 4, 8) // "word"
    applyHighlight(view, 'red')
    expect(view.state.doc.toString()).toBe('the <mark class="hl-red">word</mark> here')
  })

  it('honors an explicit range even when the live selection is collapsed', () => {
    const view = mount('the word here', 4, 8) // selection over "word"
    // Simulate a menu that stole focus and collapsed the selection.
    view.dispatch({ selection: EditorSelection.cursor(0) })
    expect(view.state.selection.main.empty).toBe(true)
    applyHighlight(view, 'red', { from: 4, to: 8 })
    expect(view.state.doc.toString()).toBe('the <mark class="hl-red">word</mark> here')
  })

  it('does nothing for an empty range and no selection', () => {
    const view = mount('the word here', 0, 0)
    applyHighlight(view, 'red', { from: 5, to: 5 })
    expect(view.state.doc.toString()).toBe('the word here')
  })

  it('yellow uses == markers', () => {
    const view = mount('the word here', 4, 8)
    applyHighlight(view, 'yellow', { from: 4, to: 8 })
    expect(view.state.doc.toString()).toBe('the ==word== here')
  })

  it('remove strips the == markers around the selected word', () => {
    const view = mount('the ==word== here', 6, 10) // inner "word", markers just outside
    applyHighlight(view, 'remove', { from: 6, to: 10 })
    expect(view.state.doc.toString()).toBe('the word here')
  })
})

describe('highlightExtension', () => {
  it('redraws when only the syntax tree changes, as when a cut parse finishes', () => {
    // Swapping the language in changes the tree and nothing else: no edit, no
    // selection, no viewport. A parse that finishes after its time budget ran
    // out reaches the plugin the same way.
    const language = new Compartment()
    const doc = '```\n==code==\n```\n\nSome ==real== text'
    const parent = document.createElement('div')
    document.body.append(parent)
    const view = new EditorView({
      parent,
      state: EditorState.create({ doc, extensions: [language.of([]), highlightExtension] })
    })
    views.push(view)
    const tinted = (): Array<string | null> =>
      Array.from(view.dom.querySelectorAll('.cm-hl')).map((e) => e.textContent)
    // No tree yet, so nothing says the fence is code.
    expect(tinted()).toEqual(['code', 'real'])
    view.dispatch({ effects: language.reconfigure(markdown({ base: markdownLanguage })) })
    forceParsing(view, doc.length, 5000)
    expect(tinted()).toEqual(['real'])
    expect(view.dom.textContent).toContain('==code==')
  })
})
