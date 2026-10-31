// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing, foldedRanges, syntaxTreeAvailable } from '@codemirror/language'
import { Compartment, EditorState, type Extension } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { afterEach, describe, expect, it } from 'vitest'
import { calloutFolding, toggleCalloutFold } from './cm-callout-fold'
import { foldAllOutline, foldAtCursor, headingFolding } from './cm-heading-fold'
import { noteMarkdown } from './cm-markdown-language'
import { wysiwygBlocksPlugin } from './cm-wysiwyg-blocks'
import { noteEditingSync } from './note-lifecycle-lock'

const DOC = [
  '# Launch', // 1
  '', // 2
  '> [!example]- Screenshots', // 3
  '> first shot', // 4
  '>', // 5
  '> second shot', // 6
  '', // 7
  '> [!tip]+ Open by default', // 8
  '> tip body', // 9
  '', // 10
  '> [!note] Plain', // 11
  '> plain body', // 12
  '', // 13
  'End.' // 14
].join('\n')

const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0))

let views: EditorView[] = []
afterEach(() => {
  for (const view of views) view.destroy()
  views = []
  document.body.innerHTML = ''
})

function mount(
  doc = DOC,
  { caretLine = 14, livePreview = new Compartment(), language = new Compartment(), parsed = true } = {}
): EditorView {
  const parent = document.createElement('div')
  document.body.append(parent)
  const state = EditorState.create({
    doc,
    extensions: [
      language.of(parsed ? markdown({ base: markdownLanguage }) : []),
      headingFolding(),
      livePreview.of([wysiwygBlocksPlugin, calloutFolding()])
    ]
  })
  const view = new EditorView({
    parent,
    state: state.update({ selection: { anchor: state.doc.line(Math.min(caretLine, state.doc.lines)).from } }).state
  })
  forceParsing(view, doc.length, 5000)
  views.push(view)
  return view
}

/** The editor's note grammar, whose tree always spans the whole note however
 *  far the parse got, and plain markdown, whose tree ends where it stopped. */
const GRAMMARS = [
  ['the note grammar', noteMarkdown],
  ['plain markdown', () => markdown({ base: markdownLanguage })]
] as const

/** Swap a markdown language in, so the tree changes and nothing else, as
 *  when the parser reaches more of a long note; then parse up to `upto`. */
function parseLater(
  view: EditorView,
  language: Compartment,
  { upto = view.state.doc.length, grammar = noteMarkdown as () => Extension } = {}
): void {
  view.dispatch({ effects: language.reconfigure(grammar()) })
  forceParsing(view, upto, 5000)
}

/** Paragraphs of filler, past where the first parse stops. */
const FILLER = Array.from({ length: 400 }, (_, i) => `Filler paragraph ${i} keeps the note long.`).join('\n\n')

/** Folded ranges as [first line, last line] pairs. */
function folds(view: EditorView): [number, number][] {
  const out: [number, number][] = []
  foldedRanges(view.state).between(0, view.state.doc.length, (from, to) => {
    out.push([view.state.doc.lineAt(from).number, view.state.doc.lineAt(to).number])
  })
  return out
}

describe('foldable callouts in the editor (#853)', () => {
  it('folds a collapsed callout when the note opens, and leaves + and plain callouts open', async () => {
    const view = mount()
    await settle()
    expect(folds(view)).toEqual([[3, 6]])
  })

  it('hides the fold marker and puts a chevron after a foldable title only', async () => {
    const view = mount()
    await settle()
    const text = view.dom.textContent ?? ''
    expect(text).not.toContain('[!example]-')
    expect(text).not.toContain('[!tip]+')
    expect(text).toContain('Screenshots')
    const chevrons = [...view.dom.querySelectorAll('.cm-callout-fold')]
    expect(chevrons.map((c) => c.textContent)).toEqual(['▸', '▾'])
    expect(chevrons[0].getAttribute('aria-expanded')).toBe('false')
    // Folded, the title line closes the card on its own.
    const head = view.dom.querySelector('.cm-callout-question.cm-callout-head')!
    expect(head.classList.contains('cm-callout-foot')).toBe(true)
  })

  it('toggles from the chevron', async () => {
    const view = mount()
    await settle()
    const chevron = () => view.dom.querySelectorAll<HTMLElement>('.cm-callout-fold')
    chevron()[0].click()
    expect(folds(view)).toEqual([])
    chevron()[1].click()
    expect(folds(view)).toEqual([[8, 9]])
    chevron()[1].click()
    expect(folds(view)).toEqual([])
  })

  it('shows no chevron on the line holding the caret, where the title is source', async () => {
    const view = mount(DOC, { caretLine: 8 })
    await settle()
    expect(view.dom.querySelectorAll('.cm-callout-fold')).toHaveLength(1)
    expect(view.dom.textContent).toContain('[!tip]+')
  })

  it('does not close again what the reader opened when live preview is reconfigured', async () => {
    const livePreview = new Compartment()
    const view = mount(DOC, { livePreview })
    await settle()
    view.dom.querySelector<HTMLElement>('.cm-callout-fold')!.click()
    expect(folds(view)).toEqual([])
    // A theme switch rebuilds the live-preview extensions.
    view.dispatch({ effects: livePreview.reconfigure([wysiwygBlocksPlugin, calloutFolding()]) })
    await settle()
    expect(folds(view)).toEqual([])
  })

  it('folds the next note’s collapsed callouts after a note swap, and not after typing', async () => {
    const view = mount()
    await settle()
    view.dom.querySelector<HTMLElement>('.cm-callout-fold')!.click()
    // Typing a marker into a title does not fold it.
    const tip = view.state.doc.line(8)
    view.dispatch({ changes: { from: tip.from + '> [!tip]'.length, to: tip.from + '> [!tip]+'.length, insert: '-' } })
    await settle()
    expect(folds(view)).toEqual([])
    // The editor swaps in another note: a whole-document replace under
    // noteEditingSync, as EditorPane does.
    const next = '> [!warning]- Later\n> hidden\n\ntext'
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
      annotations: noteEditingSync.of(true),
      selection: { anchor: next.length }
    })
    await settle()
    expect(folds(view)).toEqual([[1, 2]])
  })

  it.each(GRAMMARS)('folds a collapsed callout the parser reaches only after the note opened (%s)', async (_, grammar) => {
    const doc = `${DOC}\n\n${FILLER}\n\n> [!note]- At the end\n> end body\n\nEnd.`
    const language = new Compartment()
    const view = mount(doc, { language, parsed: false })
    await settle()
    expect(folds(view)).toEqual([])
    // The first parse covers the top of the note only.
    parseLater(view, language, { upto: 500, grammar })
    expect(syntaxTreeAvailable(view.state)).toBe(false)
    await settle()
    expect(folds(view)).toEqual([[3, 6]])
    forceParsing(view, view.state.doc.length, 5000)
    await settle()
    const end = view.state.doc.lines - 3
    expect(folds(view)).toEqual([
      [3, 6],
      [end, end + 1]
    ])
    // Opened by the reader, it stays open through later parses.
    toggleCalloutFold(view, view.state.doc.line(end).from)
    parseLater(view, language, { grammar })
    await settle()
    expect(folds(view)).toEqual([[3, 6]])
  })

  it.each(GRAMMARS)('waits for the end of a collapsed callout the parse stopped inside (%s)', async (_, grammar) => {
    const doc = `${DOC}\n\n${FILLER}\n\n> [!warning]- Long\n> first part\n>\n> second part\n\nEnd.`
    const language = new Compartment()
    const view = mount(doc, { language, parsed: false })
    await settle()
    const title = view.state.doc.lines - 5
    // Stopped inside it, the parse shows the callout ending at its first part.
    parseLater(view, language, { upto: view.state.doc.line(title).from + 4, grammar })
    expect(syntaxTreeAvailable(view.state)).toBe(false)
    await settle()
    expect(folds(view)).toEqual([[3, 6]])
    forceParsing(view, view.state.doc.length, 5000)
    await settle()
    expect(folds(view)).toEqual([
      [3, 6],
      [title, title + 3]
    ])
  })

  it.each(GRAMMARS)('folds collapsed callouts that open and end the note once a tree reaches them (%s)', async (_, grammar) => {
    const doc = '> [!note]- First\n> first body\n\nMiddle.\n\n> [!note]- Last\n> last body\n'
    const language = new Compartment()
    const view = mount(doc, { language, parsed: false, caretLine: 4 })
    await settle()
    parseLater(view, language, { grammar })
    await settle()
    expect(folds(view)).toEqual([
      [1, 2],
      [6, 7]
    ])
  })

  it('leaves open a collapsed callout typed where the parser had not reached', async () => {
    const language = new Compartment()
    const view = mount(DOC, { language, parsed: false })
    await settle()
    view.dispatch({
      changes: { from: view.state.doc.length, insert: '\n\n> [!note]- Typed\n> typed body' },
      selection: { anchor: 0 }
    })
    await settle()
    parseLater(view, language)
    await settle()
    expect(folds(view)).toEqual([[3, 6]])
  })

  it('does not fold a collapsed callout over the caret', async () => {
    const language = new Compartment()
    const view = mount(`${DOC}\n\n> [!note]- Second\n> second body`, { language, parsed: false, caretLine: 4 })
    await settle()
    parseLater(view, language)
    await settle()
    expect(folds(view)).toEqual([[16, 17]])
  })

  it('folds a callout from its title line with the fold keys, the whole body', async () => {
    const view = mount()
    await settle()
    view.dom.querySelector<HTMLElement>('.cm-callout-fold')!.click()
    view.dispatch({ selection: { anchor: view.state.doc.line(3).from + 4 } })
    expect(foldAtCursor(view)).toBe(true)
    expect(folds(view)).toEqual([[3, 6]])
  })

  it('folds a foldable callout from a line inside it and moves the caret to its title', async () => {
    const view = mount()
    await settle()
    view.dom.querySelector<HTMLElement>('.cm-callout-fold')!.click()
    view.dispatch({ selection: { anchor: view.state.doc.line(6).from + 4 } })
    expect(foldAtCursor(view)).toBe(true)
    expect(folds(view)).toEqual([[3, 6]])
    expect(view.state.doc.lineAt(view.state.selection.main.head).number).toBe(3)
  })

  it('leaves a plain callout alone from a line inside it', async () => {
    const view = mount()
    await settle()
    view.dispatch({ selection: { anchor: view.state.doc.line(12).from + 4 } })
    expect(foldAtCursor(view)).toBe(false)
  })

  it('includes foldable callouts and ordinary blockquote folds in Fold All', async () => {
    const view = mount()
    await settle()
    view.dom.querySelector<HTMLElement>('.cm-callout-fold')!.click()
    expect(foldAllOutline(view)).toBe(true)
    expect(folds(view)).toEqual([
      [1, 14],
      [3, 6],
      [8, 9],
      [11, 12]
    ])
  })
})
