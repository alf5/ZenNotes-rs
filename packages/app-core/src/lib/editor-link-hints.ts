/**
 * Hint mode's targets inside the Edit view (#894). The editor draws links as
 * text and decorations, not `<a>` elements, so the page-wide scan for
 * clickable elements finds none of them; these come from the document instead.
 */
import { syntaxTree } from '@codemirror/language'
import type { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { useStore } from '../store'
import { openWikilink } from './cm-wikilink-render'
import { followLinkTarget } from './follow-link'
import { linkRangesInLine, type LineLinkKind } from './internal-links'
import { noteEditorPaneId } from './note-editor-context'

/** A box in viewport coordinates. */
export interface HintRect {
  left: number
  top: number
  right: number
  bottom: number
}

export interface EditorLinkHint {
  view: EditorView
  paneId: string
  kind: LineLinkKind
  target: string
  /** Document offset where the link's source starts. */
  from: number
  /** The link's first drawn character, widened along its row. */
  rect: HintRect
  /** The DOM node drawing that character, so the hint can take its place in
   *  document order among the page's other targets. */
  anchor: Node
}

/**
 * True when `pos` sits inside a code span or block. `[[...]]` and URLs there
 * are literal text, drawn as code rather than as links (#248).
 */
function isInsideCode(state: EditorState, pos: number): boolean {
  let node = syntaxTree(state).resolveInner(pos, 1)
  while (node) {
    const n = node.name
    if (n === 'FencedCode' || n === 'CodeBlock' || n === 'InlineCode') return true
    if (!node.parent) break
    node = node.parent
  }
  return false
}

/**
 * The part of the screen `view` shows: its scroller, clipped to the window.
 * Null when none of it shows, as for the editor a pane in Preview mode keeps
 * mounted but hidden.
 */
function visibleBox(view: EditorView): HintRect | null {
  const box = view.scrollDOM.getBoundingClientRect()
  const left = Math.max(box.left, 0)
  const top = Math.max(box.top, 0)
  const right = Math.min(box.right, window.innerWidth)
  const bottom = Math.min(box.bottom, window.innerHeight)
  return right > left && bottom > top ? { left, top, right, bottom } : null
}

/**
 * The first character of [from, to) the editor actually draws. Live preview
 * replaces a link's syntax (`[[`, `[`, `](url)`), and a standalone image or
 * PDF line hides its whole source behind the embed, so a link's own start can
 * draw nothing at all.
 */
function firstDrawnChar(
  view: EditorView,
  from: number,
  to: number
): { pos: number; rect: HintRect } | null {
  for (let pos = from; pos < to; pos++) {
    const rect = view.coordsForChar(pos)
    if (rect && rect.right > rect.left && rect.bottom > rect.top) return { pos, rect }
  }
  return null
}

function domNodeAt(view: EditorView, pos: number): Node {
  const { node, offset } = view.domAtPos(pos)
  return node.nodeType === Node.TEXT_NODE ? node : (node.childNodes[offset] ?? node)
}

/** `first` widened to where the link ends, when that is on the same row. */
function rowRect(view: EditorView, first: HintRect, to: number): HintRect {
  const end = view.coordsAtPos(to, -1)
  const sameRow = end != null && end.top < first.bottom && end.bottom > first.top
  return {
    left: first.left,
    top: first.top,
    right: sameRow && end.right > first.right ? end.right : first.right,
    bottom: first.bottom
  }
}

/**
 * The links `view` shows on screen right now, in reading order: bare URLs,
 * `[[wikilinks]]`, and Markdown links and images, each with the target a
 * click or `gd` on it follows. A link counts only when its first drawn
 * character, where the label goes, is inside the visible part of the editor:
 * CodeMirror also draws lines beyond the scroller's edges, and folds and
 * embeds hide others.
 */
export function editorLinkHints(view: EditorView, paneId: string): EditorLinkHint[] {
  const box = visibleBox(view)
  if (!box) return []
  const { state } = view
  const hints: EditorLinkHint[] = []
  const seen = new Set<number>()
  for (const range of view.visibleRanges) {
    let line = state.doc.lineAt(range.from)
    for (;;) {
      for (const link of linkRangesInLine(line.text)) {
        // The rendered wikilink carries its target trimmed and leaves `[[ ]]`
        // as plain text, and `gd` ignores `[x]( )`: a blank target is no link.
        const target = link.target.trim()
        if (!target) continue
        const from = line.from + link.from
        if (seen.has(from)) continue
        seen.add(from)
        if (isInsideCode(state, from)) continue
        const to = line.from + link.to
        const first = firstDrawnChar(view, from, to)
        if (!first) continue
        const { rect } = first
        const middle = (rect.top + rect.bottom) / 2
        if (middle < box.top || middle > box.bottom) continue
        if (rect.right <= box.left || rect.left >= box.right) continue
        hints.push({
          view,
          paneId,
          kind: link.kind,
          target,
          from,
          rect: rowRect(view, rect, to),
          anchor: domNodeAt(view, first.pos)
        })
      }
      if (line.to >= range.to || line.number >= state.doc.lines) break
      line = state.doc.line(line.number + 1)
    }
  }
  return hints
}

/** The on-screen links of every note editor on the page, editor by editor. */
export function visibleEditorLinkHints(): EditorLinkHint[] {
  const hints: EditorLinkHint[] = []
  for (const dom of document.querySelectorAll<HTMLElement>('.cm-editor')) {
    if (dom.closest('[data-vim-hint-ignore]')) continue
    const view = EditorView.findFromDOM(dom)
    const paneId = view ? noteEditorPaneId(view) : null
    if (!view || !paneId) continue
    try {
      hints.push(...editorLinkHints(view, paneId))
    } catch {
      // CodeMirror refuses layout reads while a view is mid-update. That
      // editor's links go unlabelled instead of taking hint mode down.
    }
  }
  return hints
}

/**
 * Follow a labelled editor link the way a click follows it. A click lands in
 * its pane first (EditorPane's capture handler runs before CodeMirror sees the
 * mousedown), so a relative link resolves against that pane's note and a note
 * opens there; the label does the same, and leaves the keyboard in that
 * editor. A wikilink then resolves as a note name, as the rendered wikilink's
 * click does; a Markdown link or a bare URL takes the shared href path behind
 * the rendered Markdown link's click and every Cmd/Ctrl-click.
 */
export function followEditorLinkHint(
  hint: Pick<EditorLinkHint, 'view' | 'paneId' | 'kind' | 'target'>
): void {
  const state = useStore.getState()
  state.setActivePane(hint.paneId)
  state.setFocusedPanel('editor')
  hint.view.focus()
  if (hint.kind === 'wikilink') openWikilink(hint.target)
  else followLinkTarget(hint.target)
}
