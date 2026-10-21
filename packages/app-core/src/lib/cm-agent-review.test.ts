// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { EditorView } from '@codemirror/view'
import {
  acceptAllChanges,
  acceptChange,
  agentReviewExtension,
  isReviewing,
  nextChange,
  rejectAllChanges,
  rejectChange,
  remainingChanges
} from './cm-agent-review'
import { getCM, vim } from '@replit/codemirror-vim'

const ORIGINAL = 'one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n'
// The assistant changed line 2 and line 9.
const CURRENT = 'one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n'

let views: EditorView[] = []

function review(original = ORIGINAL, current = CURRENT, withVim = false) {
  const onDecision = vi.fn()
  const onExit = vi.fn()
  const parent = document.createElement('div')
  document.body.append(parent)
  const view = new EditorView({
    parent,
    doc: current,
    extensions: [
      ...(withVim ? [vim()] : []),
      agentReviewExtension({
        original,
        keys: {
          next: [],
          previous: [],
          accept: [],
          reject: [],
          vim: withVim
            ? { next: [[']', 'c']], previous: [['[', 'c']], accept: [['d', 'p']], reject: [['d', 'o']] }
            : undefined
        },
        onDecision,
        onExit
      })
    ]
  })
  views.push(view)
  return { view, onDecision, onExit }
}

/** Put the cursor on the line holding `text`. */
function cursorOn(view: EditorView, text: string): void {
  const at = view.state.doc.toString().indexOf(text)
  view.dispatch({ selection: { anchor: at } })
}

afterEach(() => {
  for (const view of views) view.destroy()
  views = []
})

describe('agent review mode', () => {
  it('shows each of the assistant’s changes and keeps the note read-only', () => {
    const { view } = review()
    expect(isReviewing(view.state)).toBe(true)
    expect(remainingChanges(view.state)).toBe(2)
    expect(view.state.readOnly).toBe(true)
  })

  it('accepting a change keeps the text and reports what is left', () => {
    const { view, onDecision } = review()
    cursorOn(view, 'TWO')
    expect(acceptChange(view)).toBe(true)
    expect(view.state.doc.toString()).toBe(CURRENT)
    expect(remainingChanges(view.state)).toBe(1)
    expect(onDecision).toHaveBeenLastCalledWith(ORIGINAL.replace('two', 'TWO'), 1)
  })

  it('rejecting a change puts the old text back', () => {
    const { view, onDecision } = review()
    cursorOn(view, 'NINE')
    expect(rejectChange(view)).toBe(true)
    expect(view.state.doc.toString()).toBe(CURRENT.replace('NINE', 'nine'))
    expect(remainingChanges(view.state)).toBe(1)
    expect(onDecision).toHaveBeenLastCalledWith(ORIGINAL, 1)
  })

  it('accepts or rejects everything at once', () => {
    const accepted = review()
    expect(acceptAllChanges(accepted.view)).toBe(true)
    expect(remainingChanges(accepted.view.state)).toBe(0)
    expect(accepted.onDecision).toHaveBeenLastCalledWith(CURRENT, 0)

    const rejected = review()
    expect(rejectAllChanges(rejected.view)).toBe(true)
    expect(rejected.view.state.doc.toString()).toBe(ORIGINAL)
    expect(remainingChanges(rejected.view.state)).toBe(0)
  })

  it('moves between changes', () => {
    const { view } = review()
    view.dispatch({ selection: { anchor: 0 } })
    expect(nextChange(view)).toBe(true)
    const line = view.state.doc.lineAt(view.state.selection.main.head).number
    expect(line).toBe(2)
  })

  it('leaves on Escape when nothing is selected', () => {
    const { view, onExit } = review()
    view.contentDOM.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    expect(onExit).toHaveBeenCalledTimes(1)
  })

  it('takes Vim’s diff keys ahead of the d operator', () => {
    const { view, onExit } = review(ORIGINAL, CURRENT, true)
    expect(getCM(view)).not.toBeNull()
    const key = (k: string) =>
      view.contentDOM.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }))
    cursorOn(view, 'TWO')
    key('d')
    key('o')
    expect(view.state.doc.toString()).toBe(CURRENT.replace('TWO', 'two'))
    expect(remainingChanges(view.state)).toBe(1)
    // A d cancelled with Escape does not end the review.
    key('d')
    key('Escape')
    expect(onExit).not.toHaveBeenCalled()
    key(']')
    key('c')
    expect(view.state.doc.lineAt(view.state.selection.main.head).number).toBe(9)
    key('d')
    key('p')
    expect(remainingChanges(view.state)).toBe(0)
    expect(view.state.doc.toString()).toBe(CURRENT.replace('TWO', 'two'))
  })
})
