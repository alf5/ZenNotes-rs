/**
 * Indenting leaves heading lines at the margin (#925).
 *
 * Markdown lets a heading start at most three spaces in from its container.
 * At four it is an indented code block (or, under a paragraph, more of that
 * paragraph), so Tab, Vim's `>>`, `>` and insert-mode Ctrl-T, and the phone
 * toolbar's indent button used to turn a heading into code without a word.
 * ZenNotes' outline, folding, level chips and heading motions only read
 * headings at the margin, so even the one to three spaces Markdown accepts
 * took a heading out of them. Every one of those paths indents through
 * @codemirror/commands' `indentMore` (the Vim adapter calls it too), whose
 * transactions carry the `input.indent` user event, so this filter sees them
 * all in one place and drops the insertions that land on heading lines. The
 * other lines of a selection still indent. When a press moved nothing, a
 * notice says why, so the key does not just seem dead.
 *
 * A line is held when its first character belongs to a heading: the `#` of
 * an ATX heading, or any line of a setext heading (its text and its
 * underline), at the margin or inside a list item. A list item that opens
 * with a heading (`- ## Topic`) still nests, since indenting it moves the
 * item's marker, not the heading inside it. Outdenting is untouched, so a
 * heading indented by hand can always come back.
 */
import { ensureSyntaxTree, syntaxTree } from '@codemirror/language'
import {
  EditorState,
  type ChangeSpec,
  type Extension,
  type Line
} from '@codemirror/state'
import type { SyntaxNode, Tree } from '@lezer/common'
import { useToastStore } from './toast'

const HEADING_NODE = /^(ATXHeading[1-6]|SetextHeading[12])$/

export const HEADING_INDENT_NOTICE =
  'Headings stay at the margin: indented four spaces or more, a heading turns into a code block.'

/** Whether the line's first non-blank character belongs to a heading. */
export function isHeadingLine(
  state: EditorState,
  line: Line,
  tree: Tree = syntaxTree(state)
): boolean {
  const indent = /^[ \t]*/.exec(line.text)?.[0].length ?? 0
  if (indent === line.length) return false
  for (
    let node: SyntaxNode | null = tree.resolveInner(line.from + indent, 1);
    node;
    node = node.parent
  ) {
    if (HEADING_NODE.test(node.name)) return true
  }
  return false
}

// One notice on screen at a time: pressing Tab on a heading again while it
// shows should not stack a column of them. It runs after the dispatch, and
// checks then, so presses within the same tick still add only one.
function showHeadingIndentNotice(): void {
  queueMicrotask(() => {
    const toasts = useToastStore.getState()
    if (toasts.toasts.some((toast) => toast.message === HEADING_INDENT_NOTICE)) return
    toasts.addToast(HEADING_INDENT_NOTICE, 'info')
  })
}

export function headingIndentGuard(onHeld: () => void = showHeadingIndentNotice): Extension {
  return EditorState.transactionFilter.of((tr) => {
    if (!tr.docChanged || !tr.isUserEvent('input.indent')) return tr
    const state = tr.startState
    let end = 0
    tr.changes.iterChangedRanges((_fromA, toA) => {
      end = Math.max(end, toA)
    })
    const tree = ensureSyntaxTree(state, end, 50) ?? syntaxTree(state)

    const kept: ChangeSpec[] = []
    let held = 0
    tr.changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
      const line = state.doc.lineAt(fromA)
      // indentMore only inserts at line starts; anything else passes through.
      if (fromA === toA && fromA === line.from && isHeadingLine(state, line, tree)) {
        held += 1
        return
      }
      kept.push({ from: fromA, to: toA, insert: inserted })
    })
    if (held === 0) return tr
    if (kept.length === 0) {
      onHeld()
      return {}
    }
    const changes = state.changes(kept)
    return {
      changes,
      // indentMore leaves the selection to the change mapping; an explicit one
      // is carried back to the start document and through the kept changes.
      selection: tr.selection?.map(tr.changes.invertedDesc).map(changes),
      effects: tr.effects,
      scrollIntoView: tr.scrollIntoView,
      userEvent: 'input.indent'
    }
  })
}
