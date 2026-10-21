/**
 * The little Markdown the manual's strings use (lib/help.ts): backtick code
 * spans, **bold** for the names of buttons and settings, and a blank line
 * between paragraphs. Nothing else is markup. The manual's prose mentions
 * [[ wikilinks, #tags and Markdown links as things to type, so the task
 * cards' inline-markdown renderer, which brings all three to life, is the
 * wrong one here.
 */
import type { ReactNode } from 'react'

/** A code span. Its body is literal: `inbox/**` is a glob, not bold. */
const CODE_SPAN = /(`[^`]+`)/g
/** Bold needs text that does not start or end with a space, so a lone
 *  `**` in prose stays as typed. */
const BOLD = /(\*\*[^\s*](?:[^*\n]*[^\s*])?\*\*)/g
const PARAGRAPH_BREAK = /\n{2,}/

/**
 * Short code spans become inline code; long or multi-line spans become a code
 * block so shell commands stay readable instead of wrapping through prose.
 * `codeSize` keeps inline code a step below the text around it. Paragraphs
 * are block spans rather than <p>, because a shortcut section's description
 * already sits inside a <p>; a string with one paragraph renders inline,
 * exactly as before paragraphs existed.
 */
export function renderHelpRichText(text: string, codeSize: 'text-xs' | 'text-sm'): ReactNode {
  const paragraphs: ReactNode[][] = [[]]
  text.split(CODE_SPAN).forEach((segment, index) => {
    if (index % 2 === 1) {
      paragraphs[paragraphs.length - 1]!.push(codeNode(segment.slice(1, -1), codeSize, `code-${index}`))
      return
    }
    segment.split(PARAGRAPH_BREAK).forEach((prose, part) => {
      if (part > 0) paragraphs.push([])
      paragraphs[paragraphs.length - 1]!.push(...boldNodes(prose, `text-${index}-${part}`))
    })
  })
  const filled = paragraphs.filter((nodes) => nodes.length > 0)
  if (filled.length < 2) return filled[0] ?? null
  return filled.map((nodes, index) => (
    <span key={index} className={index === 0 ? 'block' : 'mt-3 block'}>
      {nodes}
    </span>
  ))
}

/**
 * What a reader sees of a manual string, for the search: the backticks and
 * bold markers this renderer drops are gone, and whitespace runs read as one
 * space, so a phrase still matches across a bold name or a code span.
 */
export function helpSearchText(text: string): string {
  return text
    .split(CODE_SPAN)
    .map((segment, index) =>
      index % 2 === 1 ? segment.slice(1, -1) : segment.replace(BOLD, (bold) => bold.slice(2, -2))
    )
    .join('')
    .replace(/\s+/g, ' ')
}

function codeNode(code: string, codeSize: 'text-xs' | 'text-sm', key: string): ReactNode {
  if (code.includes('\n') || code.length > 60) {
    return (
      <code
        key={key}
        className="my-3 block overflow-x-auto whitespace-pre-wrap break-words rounded-lg border border-paper-300/60 bg-paper-200/60 px-3 py-2 font-mono text-xs leading-6 text-ink-800"
      >
        {code}
      </code>
    )
  }
  return (
    <code
      key={key}
      className={`rounded border border-paper-300/60 bg-paper-200/60 px-1 py-px font-mono text-ink-800 ${codeSize}`}
    >
      {code}
    </code>
  )
}

function boldNodes(prose: string, key: string): ReactNode[] {
  return prose.split(BOLD).flatMap<ReactNode>((part, index) => {
    if (index % 2 === 1) {
      return [
        <strong key={`${key}-${index}`} className="font-semibold text-ink-900">
          {part.slice(2, -2)}
        </strong>
      ]
    }
    return part ? [part] : []
  })
}
