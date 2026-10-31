// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing } from '@codemirror/language'
import { Compartment, EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { describe, expect, it } from 'vitest'
import { hashtagExtension } from './cm-hashtags'

function mount(doc: string): EditorView {
  const parent = document.createElement('div')
  document.body.append(parent)
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc,
      extensions: [markdown({ base: markdownLanguage }), hashtagExtension]
    })
  })
  forceParsing(view, doc.length, 5000)
  view.dispatch({ changes: { from: doc.length, insert: ' ' } })
  view.dispatch({ changes: { from: doc.length, to: doc.length + 1 } })
  return view
}

describe('hashtagPlugin', () => {
  it('marks inline hashtags as pills', () => {
    const view = mount('Tags: #tubex #peertube #crawler done')
    expect(view.dom.querySelectorAll('.cm-hashtag').length).toBe(3)
    view.destroy()
  })

  it('recognizes non-Latin tags — Cyrillic, CJK (#205)', () => {
    const view = mount('Заметки: #тест #ошибка и 笔记 #标签 done')
    const tags = Array.from(view.dom.querySelectorAll('.cm-hashtag')).map((e) => e.textContent)
    expect(tags).toEqual(['#тест', '#ошибка', '#标签'])
    view.destroy()
  })

  it('skips `#` inside code and headings', () => {
    const view = mount('# Heading #notatag\n\nuse `#include` here\n\n#real')
    const tags = Array.from(view.dom.querySelectorAll('.cm-hashtag')).map(
      (e) => e.textContent
    )
    expect(tags).toEqual(['#real'])
    view.destroy()
  })

  it('redraws when only the syntax tree changes, as when a cut parse finishes', () => {
    // Swapping the language in changes the tree and nothing else: no edit, no
    // selection, no viewport. A parse that finishes after its time budget ran
    // out reaches the plugin the same way.
    const language = new Compartment()
    const doc = '```c\n#include <stdio.h>\n```\n\n#real'
    const parent = document.createElement('div')
    document.body.append(parent)
    const view = new EditorView({
      parent,
      state: EditorState.create({ doc, extensions: [language.of([]), hashtagExtension] })
    })
    const tags = (): Array<string | null> =>
      Array.from(view.dom.querySelectorAll('.cm-hashtag')).map((e) => e.textContent)
    // No tree yet, so nothing says the fence is code.
    expect(tags()).toEqual(['#include', '#real'])
    view.dispatch({ effects: language.reconfigure(markdown({ base: markdownLanguage })) })
    forceParsing(view, doc.length, 5000)
    expect(tags()).toEqual(['#real'])
    view.destroy()
    parent.remove()
  })
})
