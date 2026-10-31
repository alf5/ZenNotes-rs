// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing } from '@codemirror/language'
import { Compartment, EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { describe, expect, it } from 'vitest'
import { codeBlockFlairPlugin } from './cm-code-block-flair'

describe('codeBlockFlairPlugin', () => {
  it('redraws when only the syntax tree changes, as when a cut parse finishes', () => {
    // Swapping the language in changes the tree and nothing else: no edit, no
    // selection, no viewport. A parse that finishes after its time budget ran
    // out reaches the plugin the same way.
    const language = new Compartment()
    const doc = 'Intro\n\n```js\nconst a = 1\n```\n\nEnd.'
    const parent = document.createElement('div')
    document.body.append(parent)
    const view = new EditorView({
      parent,
      state: EditorState.create({ doc, extensions: [language.of([]), codeBlockFlairPlugin] })
    })
    expect(view.dom.querySelector('.cm-code-flair')).toBeNull()
    view.dispatch({ effects: language.reconfigure(markdown({ base: markdownLanguage })) })
    forceParsing(view, doc.length, 5000)
    const flairs = Array.from(view.dom.querySelectorAll('.cm-code-flair'))
    expect(flairs.map((flair) => flair.textContent)).toEqual(['js'])
    view.destroy()
    parent.remove()
  })
})
