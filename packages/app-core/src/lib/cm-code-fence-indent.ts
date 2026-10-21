import { syntaxTree } from '@codemirror/language'
import {
  EditorSelection,
  countColumn,
  type EditorState,
  type Line,
  type StateCommand,
  type Transaction
} from '@codemirror/state'
import type { SyntaxNode } from '@lezer/common'
import { hasPendingMarkdownBlockSnippet } from './cm-markdown-snippets'

/**
 * #405: pressing Enter on a fenced-code opener that shares its line with a list
 * marker (`` - ```bash ``) must indent the new line to the fence column, so the
 * code content stays inside the list item instead of escaping to column 0 and
 * breaking the block.
 *
 * CodeMirror's `insertNewlineContinueMarkup` (the markdown Enter command)
 * continues a fenced block by copying the current line's leading *whitespace*.
 * On `` - ```bash `` that leading part is a list *marker*, not indentation, so
 * it copies nothing and the caret lands at column 0, dropping out of the block.
 *
 * This command takes over only that one case: the caret is a plain cursor on
 * the fence *opener* line, and there is non-whitespace (a list/quote marker)
 * before the fence. Every other fenced-code line (an already-indented opener,
 * the content lines, and top-level fences) is left to
 * `insertNewlineContinueMarkup`, which indents them correctly. Returning
 * `false` falls through to it.
 *
 * The one exception is a fence inside a quote or callout: there the content
 * and closing lines are taken over too (see `continueQuotedFenceLine`).
 *
 * A state command, not a view one: Vim's `o`/`O` run it on a scratch state to
 * read the prefix Enter would give a quoted line (`quotedOpenLinePrefix`).
 */
export const insertNewlineContinueFencedCodeIndent: StateCommand = ({ state, dispatch }) => {
  const range = state.selection.main
  if (!range.empty) return false

  // When the block snippet is about to auto-close the fence (it inserts the
  // closing fence, already indented), let it handle Enter instead. This is only
  // the fallback for an opener the snippet didn't register (e.g. Enter on an
  // existing fence that was not just typed).
  if (hasPendingMarkdownBlockSnippet(state)) return false

  let node: SyntaxNode | null = syntaxTree(state).resolveInner(range.head, -1)
  while (node && node.name !== 'FencedCode') node = node.parent
  if (!node) return false

  const fenceLine = state.doc.lineAt(node.from)
  const caretLine = state.doc.lineAt(range.head)
  const prefix = fenceLine.text.slice(0, node.from - fenceLine.from)
  // Past the opener, content lines already copy their own leading whitespace
  // correctly, unless the fence is in a quote.
  if (fenceLine.number !== caretLine.number) {
    return continueQuotedFenceLine(state, dispatch, node, prefix, caretLine)
  }

  // Whitespace-only (or empty) indent is already handled by the default
  // command; only a list/quote marker before the fence trips it up.
  if (!/\S/.test(prefix)) return false

  const insert = state.lineBreak + fenceContinuationPrefix(prefix)
  dispatch(
    state.update(
      state.changeByRange((r) => ({
        changes: { from: r.from, to: r.to, insert },
        range: EditorSelection.cursor(r.from + insert.length)
      })),
      { scrollIntoView: true, userEvent: 'input' }
    )
  )
  return true
}

/** Align continued lines to the fence column, turning a list marker into
 *  spaces so the content sits inside the item. A quote marker stays, or the
 *  new line would leave the quote or callout the fence is in (#961). The same
 *  rule as the block snippet's continuation prefix. */
function fenceContinuationPrefix(openerPrefix: string): string {
  return openerPrefix.replace(/[^\s>]/g, ' ')
}

/**
 * Enter on a content or closing line of a fence inside a quote or callout.
 *
 * `insertNewlineContinueMarkup` returns no markup context inside FencedCode,
 * so the default newline ran, which copies only leading whitespace. On
 * `> xy` that is nothing: the new line had no `>`, left the callout, and the
 * block broke there. Here the new line gets the opener's continuation prefix
 * plus whatever indentation this line carries past it, and otherwise behaves
 * as the default newline does: whitespace after the caret is dropped, and a
 * line left holding only its quote markup and whitespace is trimmed to the
 * markup. The closing line is handled the same way, so the caret stays in the
 * quote after the block.
 *
 * Unquoted fences and multiple carets are left to the default command. In a
 * quoted fence the code language's own indentation (after a `{`, say) is not
 * applied: it measures columns from a line start that holds the `>`.
 */
function continueQuotedFenceLine(
  state: EditorState,
  dispatch: (tr: Transaction) => void,
  fence: SyntaxNode,
  openerPrefix: string,
  line: Line
): boolean {
  if (!openerPrefix.includes('>') || state.selection.ranges.length > 1) return false
  const head = state.selection.main.head

  // Where this line's quote markup ends, read from the parse: code may itself
  // start with `>` (a `>>>` prompt), and that is not a quote marker.
  let markupEnd = -1
  for (let child = fence.firstChild; child && child.from <= line.to; child = child.nextSibling) {
    if (child.name === 'QuoteMark' && child.from >= line.from) markupEnd = child.to
  }
  // A caret inside the markup (or a line with none) is not a code position.
  if (markupEnd < 0 || head < markupEnd) return false

  let leadEnd = markupEnd
  while (leadEnd < line.to && /[\t ]/.test(line.text[leadEnd - line.from])) leadEnd++
  const lead = line.text.slice(0, leadEnd - line.from)
  const prefix = fenceContinuationPrefix(openerPrefix)
  const extra = lead.startsWith(prefix)
    ? lead.slice(prefix.length)
    : ' '.repeat(Math.max(0, countColumn(lead, state.tabSize) - countColumn(prefix, state.tabSize)))
  const insert = state.lineBreak + prefix + extra

  let to = head
  while (to < line.to && /[\t ]/.test(line.text[to - line.from])) to++
  const from = /\S/.test(state.sliceDoc(markupEnd, head)) ? head : markupEnd
  dispatch(
    state.update({
      changes: { from, to, insert },
      selection: EditorSelection.cursor(from + insert.length),
      scrollIntoView: true,
      userEvent: 'input'
    })
  )
  return true
}
