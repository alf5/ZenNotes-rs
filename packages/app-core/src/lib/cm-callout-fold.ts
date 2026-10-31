/**
 * Foldable callouts (#853), as in Obsidian: `> [!type]-` starts collapsed and
 * `> [!type]+` expanded; a callout with no marker is not foldable by click.
 *
 * The fold range is the callout's whole blockquote below its title line, so
 * the chevron, the fold keys and Fold All close the same span and any of them
 * reopens what another closed. Collapsed callouts fold once each time a note
 * opens in the editor, each once the parser has read past its end: on a long
 * note it reaches the rest only as the reader scrolls or jumps there. How far
 * it has reached rides a state field which a note swap resets (a
 * whole-document replace under `noteEditingSync`) and which outlives the
 * live-preview compartment's reconfigures: a theme switch must not close
 * again what the reader opened. A callout the reader edited, or that holds
 * the caret, is not folded.
 */
import { ensureSyntaxTree, foldEffect, syntaxTree, unfoldEffect } from '@codemirror/language'
import {
  type ChangeDesc,
  type EditorState,
  type Extension,
  StateEffect,
  StateField,
  type Transaction
} from '@codemirror/state'
import { type EditorView, ViewPlugin, type ViewUpdate, WidgetType } from '@codemirror/view'
import type { SyntaxNode, Tree } from '@lezer/common'
import { type FoldRange, foldedExactly } from './cm-list-fold'
import { noteEditingSync } from './note-lifecycle-lock'

/** A callout's title line: `> [!type]`, an optional fold marker, the title. */
export const CALLOUT_HEAD_RE = /^(\s*>\s?)\[!(\w+)\]([-+])?\s?(.*)$/

export type CalloutFoldMarker = '-' | '+'

export interface Callout {
  node: SyntaxNode
  /** `-` collapsed by default, `+` expanded, null for a plain callout. */
  marker: CalloutFoldMarker | null
  /** Everything below the title line; null when there is nothing below. */
  range: FoldRange | null
}

function calloutOf(state: EditorState, node: SyntaxNode): Callout | null {
  if (node.name !== 'Blockquote') return null
  const head = state.doc.lineAt(node.from)
  const match = head.text.match(CALLOUT_HEAD_RE)
  if (!match) return null
  return {
    node,
    marker: (match[3] as CalloutFoldMarker | undefined) ?? null,
    range: node.to > head.to ? { from: head.to, to: node.to } : null
  }
}

/** The callout whose title is on `lineNumber`, marker or not. */
export function calloutAtLine(state: EditorState, lineNumber: number): Callout | null {
  if (lineNumber < 1 || lineNumber > state.doc.lines) return null
  const line = state.doc.line(lineNumber)
  let found: Callout | null = null
  syntaxTree(state).iterate({
    from: line.from,
    to: line.to,
    enter: (ref) => {
      if (found) return false
      if (ref.name !== 'Blockquote' || ref.from < line.from || ref.from > line.to) return undefined
      found = calloutOf(state, ref.node)
      return false
    }
  })
  return found
}

/** The innermost foldable callout (one with a marker) holding `pos` below its
 *  title line. */
export function enclosingFoldableCallout(state: EditorState, pos: number): Callout | null {
  for (
    let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, -1);
    node;
    node = node.parent
  ) {
    const callout = calloutOf(state, node)
    if (callout?.marker && callout.range && pos > callout.range.from && pos <= callout.range.to) {
      return callout
    }
  }
  return null
}

/** Fold ranges of every callout with a marker. Pass a fully parsed tree
 *  (ensureSyntaxTree) to reach past the viewport. */
export function foldableCalloutRanges(state: EditorState, tree: Tree = syntaxTree(state)): FoldRange[] {
  const ranges: FoldRange[] = []
  tree.iterate({
    enter: (ref) => {
      if (ref.name !== 'Blockquote') return undefined
      const callout = calloutOf(state, ref.node)
      if (callout?.marker && callout.range) {
        ranges.push(callout.range)
      }
      return undefined
    }
  })
  return ranges
}

/** Toggle the callout whose title line holds `pos`. */
export function toggleCalloutFold(view: EditorView, pos: number): boolean {
  const { state } = view
  const callout = calloutAtLine(state, state.doc.lineAt(pos).number)
  if (!callout?.range) return false
  const existing = foldedExactly(state, callout.range)
  view.dispatch({ effects: existing ? unfoldEffect.of(existing) : foldEffect.of(callout.range) })
  return true
}

/**
 * The chevron after a foldable callout's title: ▾ open, ▸ folded. Clicking it
 * toggles the callout; the press is eaten so the caret stays where it was and
 * the title line does not turn into source under the pointer.
 */
export class CalloutFoldChevron extends WidgetType {
  constructor(private readonly folded: boolean) {
    super()
  }

  eq(other: CalloutFoldChevron): boolean {
    return other.folded === this.folded
  }

  toDOM(view: EditorView): HTMLElement {
    const chevron = document.createElement('span')
    chevron.className = `cm-callout-fold ${this.folded ? 'is-folded' : 'is-open'}`
    chevron.setAttribute('role', 'button')
    chevron.setAttribute('aria-label', this.folded ? 'Expand callout' : 'Collapse callout')
    chevron.setAttribute('aria-expanded', String(!this.folded))
    chevron.textContent = this.folded ? '▸' : '▾'
    const swallow = (event: Event): void => {
      event.preventDefault()
      event.stopPropagation()
    }
    chevron.addEventListener('mousedown', swallow)
    chevron.addEventListener('pointerdown', swallow)
    chevron.addEventListener('click', (event) => {
      swallow(event)
      toggleCalloutFold(view, view.posAtDOM(chevron))
    })
    return chevron
  }

  ignoreEvent(): boolean {
    return true
  }
}

interface Span {
  from: number
  to: number
}

/** The part of an opened note whose collapsed callouts have not been folded
 *  yet, from `from` to the end. */
interface PendingCallouts {
  from: number
  /** What changed in it since the note opened: a callout touching any of it
   *  stays open, as typing a marker does not fold. */
  edited: readonly Span[]
  /** Whether the first pass, which parses ahead, has run. */
  started: boolean
}

const calloutDefaultsReached = StateEffect.define<number>({
  map: (pos, mapping) => mapping.mapPos(pos)
})

const unopened = (): PendingCallouts => ({ from: 0, edited: [], started: false })

function replacesWholeDocument(tr: Transaction): boolean {
  let whole = false
  tr.changes.iterChangedRanges((fromA, toA) => {
    if (fromA === 0 && toA === tr.startState.doc.length) whole = true
  })
  return whole
}

function mapPending(pending: PendingCallouts, changes: ChangeDesc): PendingCallouts {
  const from = changes.mapPos(pending.from)
  const edited = pending.edited.map((span) => ({
    from: changes.mapPos(span.from, -1),
    to: changes.mapPos(span.to, 1)
  }))
  changes.iterChangedRanges((_fromA, _toA, fromB, toB) => {
    if (toB >= from) edited.push({ from: fromB, to: toB })
  })
  edited.sort((a, b) => a.from - b.from)
  const merged: Span[] = []
  for (const span of edited) {
    const last = merged[merged.length - 1]
    if (last && span.from <= last.to) last.to = Math.max(last.to, span.to)
    else merged.push(span)
  }
  return { ...pending, from, edited: merged }
}

/** Null once the parser has reached the end of the note. */
const pendingCallouts = StateField.define<PendingCallouts | null>({
  create: unopened,
  update(pending, tr) {
    if (tr.docChanged) {
      // Another note swapped in: its collapsed callouts start folded too.
      if (tr.annotation(noteEditingSync) && replacesWholeDocument(tr)) pending = unopened()
      else if (pending) pending = mapPending(pending, tr.changes)
    }
    for (const effect of tr.effects) {
      if (!pending || !effect.is(calloutDefaultsReached)) continue
      const from = Math.max(pending.from, effect.value)
      pending =
        from >= tr.state.doc.length
          ? null
          : { from, edited: pending.edited.filter((span) => span.to >= from), started: true }
    }
    return pending
  }
})

/** Where the last node the parser has produced starts, -1 before it has
 *  produced any. The tree's length does not say how far the parse got: the
 *  note grammar's tree always spans the whole note (cm-markdown-language.ts). */
function lastNodeStart(tree: Tree): number {
  let node = tree.topNode.lastChild
  if (!node) return -1
  for (let child = node.lastChild; child; child = child.lastChild) node = child
  return node.type.isTop ? -1 : node.from
}

/** Whether the parser has read past the block ending at `to`: it has produced
 *  a node after it, or only blank lines follow it. Until then the block may go
 *  on, and a parse that stopped early ends whatever is still open there. */
function readPast(state: EditorState, to: number, lastStart: number): boolean {
  if (lastStart > to) return true
  const rest = state.doc.iterRange(to)
  while (!rest.next().done) if (/\S/.test(rest.value)) return false
  return true
}

/** Fold the collapsed callouts the parser has reached since the last pass. */
function foldReachedCallouts(view: EditorView): void {
  const { state } = view
  const pending = state.field(pendingCallouts, false)
  if (!pending) return
  const tree = (!pending.started && ensureSyntaxTree(state, state.doc.length, 200)) || syntaxTree(state)
  const lastStart = lastNodeStart(tree)
  // A callout the parser has yet to produce starts on a later line.
  const read = lastStart < 0 ? 0 : state.doc.lineAt(lastStart).to + 1
  if (pending.started && read <= pending.from) return
  const caret = state.selection.main.head
  const folds: StateEffect<FoldRange>[] = []
  let reached = read
  let waiting = false
  tree.iterate({
    from: pending.from,
    enter: (ref) => {
      if (waiting) return false
      if (ref.name !== 'Blockquote' || ref.from < pending.from) return undefined
      const callout = calloutOf(state, ref.node)
      if (callout?.marker !== '-') return undefined
      // It and everything after it wait for the parser to read past its end.
      if (!readPast(state, ref.to, lastStart)) {
        reached = ref.from
        waiting = true
        return false
      }
      const { range } = callout
      if (
        range &&
        !(caret > range.from && caret <= range.to) &&
        !pending.edited.some((span) => span.from <= ref.to && span.to >= ref.from) &&
        !foldedExactly(state, range)
      ) {
        folds.push(foldEffect.of(range))
      }
      return undefined
    }
  })
  if (pending.started && folds.length === 0 && reached === pending.from) return
  view.dispatch({ effects: [...folds, calloutDefaultsReached.of(reached)] })
}

const applyCalloutDefaults = ViewPlugin.fromClass(
  class {
    private scheduled = false
    private destroyed = false

    constructor(view: EditorView) {
      this.check(view)
    }

    update(update: ViewUpdate): void {
      if (
        syntaxTree(update.startState) !== syntaxTree(update.state) ||
        update.startState.field(pendingCallouts, false) !== update.state.field(pendingCallouts, false)
      ) {
        this.check(update.view)
      }
    }

    destroy(): void {
      this.destroyed = true
    }

    private check(view: EditorView): void {
      if (this.scheduled || !view.state.field(pendingCallouts, false)) return
      this.scheduled = true
      // Not from inside a view update: CodeMirror refuses a dispatch there.
      queueMicrotask(() => {
        this.scheduled = false
        if (!this.destroyed) foldReachedCallouts(view)
      })
    }
  }
)

/** Collapsed callouts start folded when a note opens. Live preview only, with
 *  the chevrons in `cm-wysiwyg-blocks.ts`; folding itself is `codeFolding()`. */
export function calloutFolding(): Extension {
  return [pendingCallouts, applyCalloutDefaults]
}
