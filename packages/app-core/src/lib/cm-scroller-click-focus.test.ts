// @vitest-environment jsdom

import { EditorSelection, EditorState, type Extension } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { getCM, vim } from '@replit/codemirror-vim'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { scrollerClickFocus } from './cm-scroller-click-focus'

const DOC = '# Short\n\nfirst line\nsecond line'

let views: EditorView[] = []
afterEach(() => {
  for (const view of views) view.destroy()
  views = []
  document.body.innerHTML = ''
})

/** An editor whose scroller is 800 by 600 with its text ending far above the
 *  bottom. jsdom has no layout, so the size and the position CodeMirror reads
 *  for a point in the padding (the document's end) are stubbed. */
function mount(extensions: Extension[] = []): { view: EditorView; focus: ReturnType<typeof vi.fn> } {
  const view = new EditorView({
    state: EditorState.create({
      doc: DOC,
      selection: EditorSelection.cursor(3),
      extensions: [scrollerClickFocus, ...extensions]
    }),
    parent: document.body.appendChild(document.createElement('div'))
  })
  views.push(view)
  Object.defineProperty(view.scrollDOM, 'clientWidth', { value: 800 })
  Object.defineProperty(view.scrollDOM, 'clientHeight', { value: 600 })
  vi.spyOn(view, 'posAtCoords').mockReturnValue(DOC.length)
  const focus = vi.fn()
  view.focus = focus
  return { view, focus }
}

function press(target: Element, init: MouseEventInit): MouseEvent {
  const event = new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0, ...init })
  target.dispatchEvent(event)
  return event
}

describe('scrollerClickFocus (#970)', () => {
  it('puts the caret where the click points and focuses the text, not the scroller', () => {
    const { view, focus } = mount()
    const event = press(view.scrollDOM, { clientX: 120, clientY: 520 })
    // Prevented, so the browser does not give the scroller the focus.
    expect(event.defaultPrevented).toBe(true)
    expect(view.posAtCoords).toHaveBeenCalledWith({ x: 120, y: 520 }, false)
    expect(view.state.selection.main.empty).toBe(true)
    expect(view.state.selection.main.head).toBe(DOC.length)
    expect(focus).toHaveBeenCalledTimes(1)
  })

  it('extends the selection on Shift-click', () => {
    const { view } = mount()
    press(view.scrollDOM, { clientX: 120, clientY: 520, shiftKey: true })
    expect(view.state.selection.main.anchor).toBe(3)
    expect(view.state.selection.main.head).toBe(DOC.length)
  })

  it('leaves clicks on what the scroller holds to their owners', () => {
    const { view, focus } = mount()
    const gutter = view.scrollDOM.insertBefore(document.createElement('div'), view.contentDOM)
    const event = press(gutter, { clientX: 4, clientY: 40 })
    expect(event.defaultPrevented).toBe(false)
    expect(view.state.selection.main.head).toBe(3)
    expect(focus).not.toHaveBeenCalled()
  })

  it('leaves a press on the scrollbars and other buttons alone', () => {
    const { view, focus } = mount()
    const presses = [
      press(view.scrollDOM, { clientX: 805, clientY: 300 }),
      press(view.scrollDOM, { clientX: 300, clientY: 605 }),
      press(view.scrollDOM, { clientX: 120, clientY: 520, button: 2 })
    ]
    expect(presses.map((event) => event.defaultPrevented)).toEqual([false, false, false])
    expect(view.state.selection.main.head).toBe(3)
    expect(focus).not.toHaveBeenCalled()
  })

  it('keeps Vim in Normal mode', () => {
    const { view } = mount([vim()])
    press(view.scrollDOM, { clientX: 120, clientY: 520 })
    const state = getCM(view)?.state.vim as { insertMode?: boolean; visualMode?: boolean }
    expect(state.insertMode).toBeFalsy()
    expect(state.visualMode).toBeFalsy()
    expect(view.state.doc.lineAt(view.state.selection.main.head).number).toBe(4)
  })
})
