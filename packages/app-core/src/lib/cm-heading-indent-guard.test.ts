import { describe, expect, it, vi } from 'vitest'
import { indentLess, indentMore } from '@codemirror/commands'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { ensureSyntaxTree, indentUnit } from '@codemirror/language'
import { EditorSelection, EditorState, type Extension } from '@codemirror/state'
import { HEADING_INDENT_NOTICE, headingIndentGuard } from './cm-heading-indent-guard'
import { noteMarkdown } from './cm-markdown-language'
import { useToastStore } from './toast'

function stateFor(doc: string, from: number, to: number, extensions: Extension[]): EditorState {
  const state = EditorState.create({
    doc,
    selection: EditorSelection.single(from, to),
    extensions: [indentUnit.of('  '), ...extensions]
  })
  ensureSyntaxTree(state, doc.length, 5_000)
  return state
}

/** Runs Tab's command (indentMore) with the cursor or selection spanning `at`. */
function indent(doc: string, at: string, through: string = at) {
  const onHeld = vi.fn()
  const from = doc.indexOf(at)
  const to = doc.indexOf(through) + through.length
  const state = stateFor(doc, from, through === at ? from : to, [
    markdown({ base: markdownLanguage }),
    headingIndentGuard(onHeld)
  ])
  let next = state
  indentMore({ state, dispatch: (tr) => (next = tr.state) })
  return { doc: next.doc.toString(), selection: next.selection.main, onHeld }
}

describe('headingIndentGuard (#925)', () => {
  it('keeps an ATX heading at the margin and says why', () => {
    const result = indent('# Parent\n\n#### Sub one\n', 'Sub one')
    expect(result.doc).toBe('# Parent\n\n#### Sub one\n')
    expect(result.onHeld).toHaveBeenCalledTimes(1)
  })

  it('does not push a heading indented by hand any further', () => {
    expect(indent('  #### Sub one\n', 'Sub one').doc).toBe('  #### Sub one\n')
  })

  it('holds both lines of a setext heading', () => {
    const doc = 'Setext title\n------------\n\nbody\n'
    expect(indent(doc, 'Setext title').doc).toBe(doc)
    expect(indent(doc, '------------').doc).toBe(doc)
  })

  it('holds a heading inside a list item', () => {
    const doc = '- item\n  #### In item\n'
    expect(indent(doc, 'In item').doc).toBe(doc)
  })

  it('indents a paragraph as before, without a notice', () => {
    const result = indent('Some text\n', 'Some text')
    expect(result.doc).toBe('  Some text\n')
    expect(result.onHeld).not.toHaveBeenCalled()
  })

  it('still nests a list item that opens with a heading', () => {
    expect(indent('- one\n- ## Topic\n', '## Topic').doc).toBe('- one\n  - ## Topic\n')
  })

  it('indents a # comment inside a fenced code block', () => {
    expect(indent('```\n# comment\n```\n', '# comment').doc).toBe('```\n  # comment\n```\n')
  })

  it('indents the rest of a selection around headings, without a notice', () => {
    const result = indent('## A\nalpha\n## B\nbeta\n', '## A', 'beta')
    expect(result.doc).toBe('## A\n  alpha\n## B\n  beta\n')
    expect(result.onHeld).not.toHaveBeenCalled()
  })

  it('leaves the cursor where it was when nothing moved', () => {
    const doc = '#### Sub one\n'
    const result = indent(doc, 'one')
    expect(result.selection.head).toBe(doc.indexOf('one'))
  })

  it('lets a heading indented by hand come back to the margin', () => {
    const state = stateFor('  ## Back\n', 4, 4, [
      markdown({ base: markdownLanguage }),
      headingIndentGuard(vi.fn())
    ])
    let next = state
    indentLess({ state, dispatch: (tr) => (next = tr.state) })
    expect(next.doc.toString()).toBe('## Back\n')
  })

  it('only guards indenting: typed spaces still reach the line', () => {
    const state = stateFor('## Typed\n', 0, 0, [
      markdown({ base: markdownLanguage }),
      headingIndentGuard(vi.fn())
    ])
    const next = state.update({ changes: { from: 0, insert: '    ' }, userEvent: 'input.type' }).state
    expect(next.doc.toString()).toBe('    ## Typed\n')
  })

  it('comes with every note editor through noteMarkdown(), notice included', async () => {
    useToastStore.setState({ toasts: [] })
    const doc = '---\ntitle: x\n---\n# Heading\n'
    const state = stateFor(doc, doc.indexOf('Heading'), doc.indexOf('Heading'), [noteMarkdown()])
    let next = state
    indentMore({ state, dispatch: (tr) => (next = tr.state) })
    expect(next.doc.toString()).toBe(doc)
    // Pressing again while the notice shows does not stack a second one.
    indentMore({ state: next, dispatch: (tr) => (next = tr.state) })
    await Promise.resolve()
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual([HEADING_INDENT_NOTICE])

    // A frontmatter line is not a heading, even though plain markdown would
    // read `title: x` over the closing fence as a setext heading.
    const fm = stateFor(doc, doc.indexOf('title'), doc.indexOf('title'), [noteMarkdown()])
    let afterFm = fm
    indentMore({ state: fm, dispatch: (tr) => (afterFm = tr.state) })
    expect(afterFm.doc.toString()).toBe('---\n  title: x\n---\n# Heading\n')
  })
})
