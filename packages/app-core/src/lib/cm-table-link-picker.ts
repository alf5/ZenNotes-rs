/**
 * The `[[` note picker for WYSIWYG table cells (#977). A cell is a plain
 * contenteditable inside an atomic block widget, so its keystrokes never become
 * editor transactions and CodeMirror's completion sources never see them: `[[`
 * in a cell opened nothing, and links in tables had to be typed from memory.
 *
 * This is the same picker on the cell's own text: the editor's ranking
 * (`wikilinkCandidates`), the editor's item rendering and popup classes (so it
 * looks the same and the app's "a completion owns the keyboard" checks see it),
 * and the editor's keys: arrows and Ctrl+J/K/N/P move, Enter or Ctrl+Y insert,
 * Tab inserts with the caret left inside the brackets, `|` inserts and starts
 * the display text, Escape closes. The edits are the editor's edits measured on
 * the cell string; the widget writes them into the cell and commits on blur
 * like any other cell edit. Headings (`[[note#`) and block refs stay
 * editor-only.
 */
import type { Completion } from '@codemirror/autocomplete'
import type { EditorView } from '@codemirror/view'
import type { LinkCandidate } from './link-candidates'
import { wikilinkCandidates } from './cm-wikilinks'
import { slashCommandRender } from './cm-slash-commands'
import { completionNavDirection, isTypedPipe } from './cm-completion-nav'

export interface CellLinkQuery {
  /** Offset of the `[[` in the cell text. */
  open: number
  query: string
  /** A `!` sits right before the `[[`, so the link is already an embed. */
  bang: boolean
}

/** A cell's next text and the selection to restore in it. */
export interface CellTextEdit {
  text: string
  from: number
  to: number
}

/**
 * True when `at` falls inside an inline code span: a run of N backticks opens
 * one that the next run of exactly N closes, and a run with no closer is plain
 * text. `[[` in code is literal, so no picker opens for it (the editor's #783).
 */
function insideCodeSpan(text: string, at: number): boolean {
  const runs = [...text.matchAll(/`+/g)]
  for (let i = 0; i < runs.length; i++) {
    const open = runs[i]
    if ((open.index ?? 0) >= at) return false
    const close = runs.findIndex((run, j) => j > i && run[0].length === open[0].length)
    if (close < 0) continue
    if ((runs[close].index ?? 0) > at) return true
    i = close
  }
  return false
}

/**
 * The unclosed `[[query` right before `caret`, by the editor's rules: nothing
 * once the link has a `]]`, a `|` display text, or a `#`/`^` anchor.
 */
export function cellLinkQueryAt(text: string, caret: number): CellLinkQuery | null {
  const before = text.slice(0, caret)
  const open = before.lastIndexOf('[[')
  if (open < 0) return null
  const inside = before.slice(open + 2)
  if (inside.includes(']]') || inside.includes('|')) return null
  if (inside.includes('#') || inside.includes('^')) return null
  if (insideCodeSpan(text, open)) return null
  return { open, query: inside, bang: open > 0 && text[open - 1] === '!' }
}

/** The text between `pos` and the `]]` that closes the link `pos` sits in, or
 *  null while that link is still being typed (`closedLinkTail` for a string). */
function closedTail(text: string, pos: number): string | null {
  const after = text.slice(pos)
  const close = after.indexOf(']]')
  if (close < 0) return null
  const tail = after.slice(0, close)
  return tail.includes('[[') ? null : tail
}

/**
 * The cell after picking `candidate` for the `[[query` at `caret`, the edit
 * the editor's picker makes: a link still being typed is closed and the caret
 * lands past its `]]`; inside a closed link only the note segment is replaced,
 * so a `#section` or `|alias` after it survives (#686). An asset becomes an
 * embed.
 */
export function cellLinkPick(
  text: string,
  caret: number,
  match: CellLinkQuery,
  candidate: Pick<LinkCandidate, 'kind' | 'target'>
): CellTextEdit {
  const from = match.open + 2
  const tail = closedTail(text, caret)
  let insert = candidate.target
  let end = caret
  if (tail === null) {
    insert += ']]'
  } else {
    let stop = tail.length
    for (const ch of '#^|') {
      const at = tail.indexOf(ch)
      if (at >= 0 && at < stop) stop = at
    }
    end = caret + stop
  }
  let next = text.slice(0, from) + insert + text.slice(end)
  let cursor = from + insert.length
  if (candidate.kind === 'asset' && !match.bang) {
    next = `${next.slice(0, match.open)}!${next.slice(match.open)}`
    cursor += 1
  }
  return { text: next, from: cursor, to: cursor }
}

/** Tab's pick: the caret stays inside the brackets, ready for a `#heading`. */
export function keepCaretInsideLink(edit: CellTextEdit): CellTextEdit {
  if (edit.text.slice(edit.from - 2, edit.from) !== ']]') return edit
  return { ...edit, from: edit.from - 2, to: edit.from - 2 }
}

/** `|`'s pick (#804): a `|` before the `]]` with the caret behind it, or the
 *  existing display text selected when the link already has one. */
export function startDisplayText(edit: CellTextEdit): CellTextEdit {
  const { text, from: pos } = edit
  const inside = text.slice(pos - 2, pos) === ']]' ? pos - 2 : pos
  const tail = closedTail(text, inside) ?? ''
  const pipe = tail.indexOf('|')
  if (pipe >= 0) return { text, from: inside + pipe + 1, to: inside + tail.length }
  const at = inside + tail.length
  return { text: `${text.slice(0, at)}|${text.slice(at)}`, from: at + 1, to: at + 1 }
}

type PickMode = 'close' | 'inside' | 'display'

/** Viewport rect of the character offset `offset` in `el`'s text. */
function rectAtOffset(el: HTMLElement, offset: number): DOMRect {
  const walker = el.ownerDocument.createTreeWalker(el, NodeFilter.SHOW_TEXT)
  let left = offset
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const length = node.textContent?.length ?? 0
    if (left <= length) {
      const range = el.ownerDocument.createRange()
      range.setStart(node, left)
      range.collapse(true)
      const rect = range.getClientRects?.()[0]
      if (rect) return rect
      break
    }
    left -= length
  }
  return el.getBoundingClientRect()
}

export class TableCellLinkPicker {
  private container: HTMLElement | null = null
  private list: HTMLUListElement | null = null
  private cell: HTMLElement | null = null
  private match: CellLinkQuery | null = null
  private items: LinkCandidate[] = []
  private selected = 0
  private lastRead = ''
  private readonly onSelectionChange = (): void => {
    if (this.cell) this.update(this.cell)
  }
  private readonly onScroll = (): void => this.position()

  constructor(
    private readonly view: EditorView,
    /** The cell's source and collapsed caret, or null when it is not being
     *  typed in (Vim NORMAL, a range selected, focus elsewhere). */
    private readonly read: (cell: HTMLElement) => { text: string; caret: number } | null,
    private readonly apply: (cell: HTMLElement, edit: CellTextEdit) => void
  ) {}

  get isOpen(): boolean {
    return this.container !== null
  }

  /** Open, refilter, or close for the `[[` at the caret of `cell`. */
  update(cell: HTMLElement): void {
    const live = cell.isConnected ? this.read(cell) : null
    const key = live ? `${live.caret}\u0000${live.text}` : ''
    if (this.isOpen && this.cell === cell && key === this.lastRead) return
    this.lastRead = key
    const match = live ? cellLinkQueryAt(live.text, live.caret) : null
    const items = match ? wikilinkCandidates(match.query) : []
    if (!match || items.length === 0) {
      this.close()
      return
    }
    const sameQuery = this.isOpen && this.match?.open === match.open && this.match.query === match.query
    this.cell = cell
    this.match = match
    if (!sameQuery) {
      this.items = items
      this.selected = 0
    }
    this.render()
  }

  /** The picker's keys while it is open; true when the key was spent. */
  handleKey(event: KeyboardEvent): boolean {
    if (!this.isOpen || event.isComposing) return false
    const plain = !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey
    const direction =
      plain && event.key === 'ArrowDown'
        ? 'next'
        : plain && event.key === 'ArrowUp'
          ? 'previous'
          : completionNavDirection(event)
    if (direction) {
      this.move(direction === 'next' ? 1 : -1)
      return true
    }
    if (event.key === 'Escape') {
      this.close()
      return true
    }
    const ctrlY =
      event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey && event.key.toLowerCase() === 'y'
    if ((plain && event.key === 'Enter') || ctrlY) return this.pick('close')
    if (plain && event.key === 'Tab') return this.pick('inside')
    if (isTypedPipe(event)) return this.pick('display')
    return false
  }

  close(): void {
    if (!this.container) return
    this.container.remove()
    this.container = null
    this.list = null
    this.cell = null
    this.match = null
    this.items = []
    this.lastRead = ''
    document.removeEventListener('selectionchange', this.onSelectionChange)
    window.removeEventListener('scroll', this.onScroll, true)
  }

  /** Insert the highlighted suggestion, re-reading the cell at key time: a
   *  stale list must not write over text the caret has left. */
  private pick(mode: PickMode, index = this.selected): boolean {
    const cell = this.cell
    const candidate = this.items[index]
    const live = cell ? this.read(cell) : null
    const match = live ? cellLinkQueryAt(live.text, live.caret) : null
    this.close()
    if (!cell || !candidate || !live || !match) return false
    let edit = cellLinkPick(live.text, live.caret, match, candidate)
    if (mode === 'inside') edit = keepCaretInsideLink(edit)
    if (mode === 'display') edit = startDisplayText(edit)
    this.apply(cell, edit)
    return true
  }

  private move(delta: number): void {
    const count = this.items.length
    if (count === 0) return
    this.selected = (this.selected + delta + count) % count
    this.list?.querySelectorAll('li').forEach((li, i) => {
      if (i === this.selected) {
        li.setAttribute('aria-selected', 'true')
        li.scrollIntoView?.({ block: 'nearest' })
      } else {
        li.removeAttribute('aria-selected')
      }
    })
  }

  private render(): void {
    if (!this.container) {
      // Mounted the way CodeMirror mounts its tooltips on <body>: a wrapper
      // with the editor's theme classes, so the autocomplete theme applies.
      const container = document.createElement('div')
      container.className = this.view.themeClasses
      container.style.position = 'relative'
      const tooltip = document.createElement('div')
      tooltip.className = 'cm-tooltip cm-tooltip-autocomplete'
      tooltip.style.position = 'fixed'
      const list = document.createElement('ul')
      list.setAttribute('role', 'listbox')
      list.setAttribute('aria-label', 'Link suggestions')
      // mousedown, as CodeMirror does: the cell keeps focus and its caret.
      list.addEventListener('mousedown', (event) => {
        const li = (event.target as HTMLElement | null)?.closest('li')
        if (!li || event.button !== 0) return
        event.preventDefault()
        this.pick('close', [...list.children].indexOf(li))
      })
      tooltip.append(list)
      container.append(tooltip)
      document.body.append(container)
      this.container = container
      this.list = list
      document.addEventListener('selectionchange', this.onSelectionChange)
      window.addEventListener('scroll', this.onScroll, true)
    }
    const list = this.list as HTMLUListElement
    list.replaceChildren(
      ...this.items.map((candidate, i) => {
        const li = document.createElement('li')
        li.setAttribute('role', 'option')
        li.className = 'wikilink-cmd-option'
        if (i === this.selected) li.setAttribute('aria-selected', 'true')
        const option = {
          label: candidate.label,
          detail: candidate.subtitle,
          _kind: 'wikilink',
          _subtitle: candidate.subtitle
        } as Completion
        li.append(slashCommandRender.render(option))
        return li
      })
    )
    this.position()
  }

  /** Under the query, where the editor anchors its popup, or above it when
   *  the viewport has no room underneath. */
  private position(): void {
    const tooltip = this.container?.firstElementChild as HTMLElement | null
    if (!tooltip || !this.cell || !this.match) return
    const anchor = rectAtOffset(this.cell, this.match.open + 2)
    const height = tooltip.offsetHeight
    const width = tooltip.offsetWidth
    const below = anchor.bottom + 4
    const top =
      below + height > window.innerHeight - 4 && anchor.top - height - 4 >= 4
        ? anchor.top - height - 4
        : below
    const left = Math.max(4, Math.min(anchor.left, window.innerWidth - width - 4))
    tooltip.style.top = `${top}px`
    tooltip.style.left = `${left}px`
  }
}
