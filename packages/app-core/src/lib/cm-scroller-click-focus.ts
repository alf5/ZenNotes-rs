import { EditorSelection } from '@codemirror/state'
import { ViewPlugin, type EditorView } from '@codemirror/view'

/**
 * A click in the editor's empty space places the caret and keeps the keyboard
 * in the editor. The scroller pads 20vh under the text and centers a capped
 * text column, so the space below a short note, above it and beside the
 * column belongs to `.cm-scroller`, not to the content. CodeMirror listens on
 * the content only, so it never saw those clicks, and the browser gave their
 * focus to the scroller (focusable, tabindex -1), where no key handler runs:
 * Vim's keys did nothing and the editor looked frozen until a click on the
 * text (#970). The caret goes where CodeMirror puts it for coordinates there:
 * the document's end below the text, its start above, the nearest column of
 * that row beside it.
 */
export const scrollerClickFocus = ViewPlugin.fromClass(
  class {
    constructor(readonly view: EditorView) {
      view.scrollDOM.addEventListener('mousedown', this.onMouseDown)
    }

    onMouseDown = (event: MouseEvent): void => {
      const { view } = this
      const scroller = view.scrollDOM
      // Gutters, the content and its widgets handle their own clicks.
      if (event.button !== 0 || event.target !== scroller || event.defaultPrevented) return
      // A press on the scroller's own scrollbars stays a scrollbar drag.
      const rect = scroller.getBoundingClientRect()
      if (event.clientX >= rect.left + scroller.clientLeft + scroller.clientWidth) return
      if (event.clientY >= rect.top + scroller.clientTop + scroller.clientHeight) return
      event.preventDefault()
      const pos = view.posAtCoords({ x: event.clientX, y: event.clientY }, false)
      const main = view.state.selection.main
      view.dispatch({
        selection: event.shiftKey ? EditorSelection.range(main.anchor, pos) : EditorSelection.cursor(pos),
        userEvent: 'select.pointer'
      })
      view.focus()
    }

    destroy(): void {
      this.view.scrollDOM.removeEventListener('mousedown', this.onMouseDown)
    }
  }
)
