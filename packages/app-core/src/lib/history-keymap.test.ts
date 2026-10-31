// @vitest-environment jsdom

import { vi } from 'vitest'

// CodeMirror picks its platform when @codemirror/view loads, so Windows has to
// be in place before the imports below run.
vi.hoisted(() => {
  Object.defineProperty(navigator, 'platform', { value: 'Win32', configurable: true })
})

import { history, historyKeymap, undo } from '@codemirror/commands'
import { EditorState, type Extension } from '@codemirror/state'
import { EditorView, keymap, runScopeHandlers, type KeyBinding } from '@codemirror/view'
import { afterEach, describe, expect, it } from 'vitest'
import { appHistoryKeymap } from './history-keymap'

let views: EditorView[] = []
afterEach(() => {
  for (const view of views) view.destroy()
  views = []
  document.body.innerHTML = ''
})

/** An editor whose last edit was just undone, so a redo has work to do. */
function undoneEditor(bindings: readonly KeyBinding[]): EditorView {
  const extensions: Extension[] = [history(), keymap.of(bindings)]
  const view = new EditorView({
    state: EditorState.create({ doc: 'hello', extensions }),
    parent: document.body.appendChild(document.createElement('div'))
  })
  views.push(view)
  view.dispatch({ changes: { from: 5, insert: ' world' }, userEvent: 'input.type' })
  undo(view)
  expect(view.state.doc.toString()).toBe('hello')
  return view
}

function press(view: EditorView, key: string, keyCode: number, init: KeyboardEventInit): boolean {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init })
  Object.defineProperty(event, 'keyCode', { value: keyCode })
  return runScopeHandlers(view, event, 'editor')
}

describe('appHistoryKeymap on Windows (#967)', () => {
  it("is a platform CodeMirror's own keymap leaves without Ctrl+Shift+Z", () => {
    const view = undoneEditor(historyKeymap)
    expect(press(view, 'Z', 90, { ctrlKey: true, shiftKey: true })).toBe(false)
    expect(view.state.doc.toString()).toBe('hello')
  })

  it('redoes on Ctrl+Shift+Z, the shortcut the context menu shows', () => {
    const view = undoneEditor(appHistoryKeymap)
    expect(press(view, 'Z', 90, { ctrlKey: true, shiftKey: true })).toBe(true)
    expect(view.state.doc.toString()).toBe('hello world')
  })

  it('keeps Ctrl+Y redoing too', () => {
    const view = undoneEditor(appHistoryKeymap)
    expect(press(view, 'y', 89, { ctrlKey: true })).toBe(true)
    expect(view.state.doc.toString()).toBe('hello world')
  })
})
