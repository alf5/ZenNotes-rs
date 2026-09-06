import { syntaxTree } from '@codemirror/language'
import { Facet, RangeSetBuilder } from '@codemirror/state'
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate
} from '@codemirror/view'
import { DEFAULT_EDITOR_TAB_SIZE, normalizeEditorTabSize } from './editor-tab-size'

// The checkbox part covers every task state (`[ ]`, `[x]`, `[>]`, `[-]`, `[/]`)
// so Tab/Shift-Tab indents a started or cancelled task like any other. (#512)
const LEADING_LIST_MARKER_RE =
  /^[ \t]*(?:[-+*]|\d{1,9}[.)])(?:[ \t]+|$)(?:\[[ xX>/-]\](?:[ \t]+|$))?/
const LIST_MARKER_FROM_OFFSET_RE =
  /^(?:[-+*]|\d{1,9}[.)])(?:[ \t]+|$)(?:\[[ xX>/-]\](?:[ \t]+|$))?/

/**
 * How list indentation renders (#491). `width` is the visual step per nesting
 * level in ch, fed by the Tab size setting so one knob governs what Tab
 * inserts AND how deep a level looks; `guides` draws a vertical line per
 * ancestor level. Combined here so panes can provide the two halves from
 * separate settings through one compartment.
 */
export interface ListIndentConfig {
  width: number
  guides: boolean
}

export const listIndentConfig = Facet.define<Partial<ListIndentConfig>, ListIndentConfig>({
  combine: (values) => ({
    width: values.find((v) => v.width != null)?.width ?? DEFAULT_EDITOR_TAB_SIZE,
    guides: values.find((v) => v.guides != null)?.guides ?? true
  })
})

/** Provide the guides half of the config. */
export function listIndentGuides(enabled: boolean): ReturnType<typeof listIndentConfig.of> {
  return listIndentConfig.of({ guides: enabled })
}

/** Provide the width half, normalized exactly like the Tab size setting. */
export function listIndentWidth(value: unknown): ReturnType<typeof listIndentConfig.of> {
  return listIndentConfig.of({ width: normalizeEditorTabSize(value) })
}

function visualColumn(text: string, tabWidth: number): number {
  let col = 0
  for (const ch of text) col += ch === '\t' ? tabWidth : 1
  return col
}

export function markdownListHangingIndentCh(
  lineText: string,
  markerOffset = 0,
  tabWidth = DEFAULT_EDITOR_TAB_SIZE
): number | null {
  if (markerOffset < 0 || markerOffset > lineText.length) return null
  const markerText = lineText.slice(markerOffset)
  const match =
    markerOffset === 0
      ? lineText.match(LEADING_LIST_MARKER_RE)
      : markerText.match(LIST_MARKER_FROM_OFFSET_RE)
  if (!match) return null
  return Math.max(1, visualColumn(lineText.slice(0, markerOffset) + match[0], tabWidth))
}

/**
 * The extra left padding (ch) that widens a nested line's indent to
 * `depth * width`, whatever the source spacing. A source already indented at
 * least that far gets 0, so notes written with the configured width never
 * shift, and top-level lines never shift.
 */
export function listIndentPadCh(
  lineText: string,
  markerOffset: number,
  depth: number,
  width: number
): number {
  const markerCol = visualColumn(lineText.slice(0, markerOffset), width)
  return Math.max(0, (depth - 1) * width - markerCol)
}

/**
 * Background layers for the guide lines of a line at `depth`: one 1px column
 * per ancestor level, at that level's marker column plus a small inset so the
 * line sits under the bullet, not the left edge. Positions are in ch, so they
 * track the editor font. Empty at depth <= 1.
 */
export function listGuideStyle(depth: number, width: number): string {
  if (depth <= 1) return ''
  const images: string[] = []
  const positions: string[] = []
  for (let level = 1; level < depth; level++) {
    images.push('linear-gradient(to bottom, var(--z-list-guide-color) 0, var(--z-list-guide-color) 100%)')
    positions.push(`calc(${(level - 1) * width}ch + 0.45ch) 0`)
  }
  return (
    `background-image: ${images.join(', ')}; ` +
    `background-position: ${positions.join(', ')}; ` +
    'background-size: 1px 100%; background-repeat: no-repeat;'
  )
}

interface ListLineFacts {
  markerOffset: number
  /** 1-based nesting level: ListItem ancestors of the marker. */
  depth: number
}

function listLineFactsFor(view: EditorView, lineFrom: number, lineTo: number): ListLineFacts | null {
  let facts: ListLineFacts | null = null
  syntaxTree(view.state).iterate({
    from: lineFrom,
    to: lineTo,
    enter: (node) => {
      if (facts != null) return false
      if (node.name !== 'ListMark') return
      let depth = 0
      for (let p = node.node.parent; p; p = p.parent) {
        if (p.name === 'ListItem') depth++
      }
      facts = { markerOffset: node.from - lineFrom, depth: Math.max(1, depth) }
      return false
    }
  })
  return facts
}

function computeDecorations(view: EditorView): DecorationSet {
  const { width, guides } = view.state.facet(listIndentConfig)
  const builder = new RangeSetBuilder<Decoration>()
  const decoratedLines = new Set<number>()

  for (const { from, to } of view.visibleRanges) {
    const firstLine = view.state.doc.lineAt(from).number
    const lastLine = view.state.doc.lineAt(Math.max(from, to - 1)).number
    for (let lineNo = firstLine; lineNo <= lastLine; lineNo++) {
      if (decoratedLines.has(lineNo)) continue
      const line = view.state.doc.line(lineNo)
      const facts = listLineFactsFor(view, line.from, line.to)
      if (facts == null) continue
      const indentCh = markdownListHangingIndentCh(line.text, facts.markerOffset, width)
      if (indentCh == null) continue
      decoratedLines.add(lineNo)
      const padCh = listIndentPadCh(line.text, facts.markerOffset, facts.depth, width)
      const style = [
        `--z-list-hanging-indent: ${indentCh}ch`,
        padCh > 0 ? `--z-list-indent-pad: ${padCh}ch` : '',
        guides ? listGuideStyle(facts.depth, width) : ''
      ]
        .filter(Boolean)
        .join('; ')
      builder.add(
        line.from,
        line.from,
        Decoration.line({
          class: 'cm-markdown-list-line',
          attributes: { style }
        })
      )
    }
  }

  return builder.finish()
}

export const markdownListIndentPlugin = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet

    constructor(view: EditorView) {
      this.decorations = computeDecorations(view)
    }

    update(update: ViewUpdate): void {
      if (
        update.docChanged ||
        update.viewportChanged ||
        update.state.facet(listIndentConfig) !== update.startState.facet(listIndentConfig)
      ) {
        this.decorations = computeDecorations(update.view)
      }
    }
  },
  {
    decorations: (plugin) => plugin.decorations
  }
)
