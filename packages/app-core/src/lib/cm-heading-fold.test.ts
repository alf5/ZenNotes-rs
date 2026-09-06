// @vitest-environment jsdom

import { describe, it, expect } from 'vitest'
import { EditorState } from '@codemirror/state'
import { foldService } from '@codemirror/language'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { EditorView } from '@codemirror/view'
import {
  foldHeadingAtCursor,
  headingFolding,
  unfoldHeadingAtCursor
} from './cm-heading-fold'

/** Run the heading fold service over a given line; returns its fold range. */
function foldRangeAtLine(doc: string, lineNumber: number): { from: number; to: number } | null {
  const state = EditorState.create({
    doc,
    extensions: [markdown({ base: markdownLanguage }), headingFolding()]
  })
  const line = state.doc.line(lineNumber)
  for (const svc of state.facet(foldService)) {
    const range = svc(state, line.from, line.to)
    if (range) return range
  }
  return null
}

describe('heading folding', () => {
  it('treats a real heading line as foldable', () => {
    const doc = '# Real heading\n\nbody text\nmore\n'
    expect(foldRangeAtLine(doc, 1)).not.toBeNull()
  })

  it('does NOT treat a `#` comment inside a fenced code block as a heading (#83)', () => {
    // `# This is a comment` is line 3 inside the ```bash fence.
    const doc = '```bash\n#!/bin/bash\n# This is a comment\necho "Hello"\n```\n'
    expect(foldRangeAtLine(doc, 3)).toBeNull()
  })

  it('does NOT fold a `#` line inside a plain (unlabelled) fence', () => {
    const doc = '```\n# not a heading\nplain\n```\n'
    expect(foldRangeAtLine(doc, 2)).toBeNull()
  })

  it('still folds a real heading that follows a code block', () => {
    const doc = '```\n# in code\n```\n\n# Real\n\nbody\n'
    expect(foldRangeAtLine(doc, 5)).not.toBeNull()
  })

  it('shows the heading level before a heading when labels are enabled', () => {
    const parent = document.createElement('div')
    document.body.append(parent)
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: '## Design\n\nDetails',
        extensions: [
          markdown({ base: markdownLanguage }),
          headingFolding({ showLevelLabels: true })
        ]
      })
    })

    expect(parent.querySelector('.cm-heading-level-label')?.textContent).toBe('H2')

    view.destroy()
    parent.remove()
  })

  it('keeps heading level labels out of the editor by default', () => {
    const parent = document.createElement('div')
    document.body.append(parent)
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: '# Design\n\nDetails',
        extensions: [markdown({ base: markdownLanguage }), headingFolding()]
      })
    })

    expect(parent.querySelector('.cm-heading-level-label')).toBeNull()

    view.destroy()
    parent.remove()
  })

  it('folds and unfolds the heading at the cursor through commands', () => {
    const parent = document.createElement('div')
    document.body.append(parent)
    const doc = '# Top\n\nIntro\n\n## Fold me\n\nBody\n\n## Next\n'
    const cursor = doc.indexOf('## Fold me')
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc,
        selection: { anchor: cursor },
        extensions: [markdown({ base: markdownLanguage }), headingFolding()]
      })
    })

    expect(foldHeadingAtCursor(view)).toBe(true)
    expect(
      Array.from(parent.querySelectorAll('.cm-heading-line-folded')).some((line) =>
        line.textContent?.includes('Fold me')
      )
    ).toBe(true)
    expect(unfoldHeadingAtCursor(view)).toBe(true)
    expect(parent.querySelector('.cm-heading-line-folded')).toBeNull()

    view.destroy()
    parent.remove()
  })
})
