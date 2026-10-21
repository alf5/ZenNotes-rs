// A lone `-` typed under a line of text is how a bullet list starts, but
// CommonMark reads it as a setext underline and turns the line above into a
// level-2 heading (#898). ZenNotes keeps that line a paragraph, with the dash
// as its last line, and ends the paragraph there, exactly where the setext
// heading would have ended, so the lines after it parse the same either way.
// Two or more dashes, and `=` underlines, are still headings.
//
// The rule lives in four places that must agree: the editor grammar
// (app-core cm-markdown-language.ts), the outline (app-core outline.ts), every
// remark renderer (this plugin: Preview, PDF, email, DOCX, share viewer, Quick
// Look), and the zn CLI's outline (ZenNotes/tui internal/vault/parse.go).

// Structural mdast, so shared-domain needs neither unified nor @types/mdast.
interface MdastNode {
  type: string
  children?: MdastNode[]
  value?: string
  depth?: number
  data?: unknown
}

interface FromMarkdownContext {
  stack: MdastNode[]
  sliceSerialize(token: unknown): string
}

const SINGLE_DASH_FLAG = 'zenSingleDashUnderline'

type NodeData = Record<string, unknown> | undefined

/**
 * Remark plugin: a setext heading underlined by a single `-` comes back as the
 * paragraph it was typed as. Register it on any pipeline that parses note
 * markdown, before plugins that look at headings.
 */
export function remarkSingleDashParagraph(this: { data(): object }): (tree: MdastNode) => void {
  const data = this.data() as { fromMarkdownExtensions?: unknown[] }
  const fromMarkdown = (data.fromMarkdownExtensions ??= [])
  fromMarkdown.push({
    enter: {
      // The heading is the open node while its underline is read; the token
      // covers the dashes only, not indentation or trailing space.
      setextHeadingLineSequence(this: FromMarkdownContext, token: unknown): void {
        if (this.sliceSerialize(token) !== '-') return
        const heading = this.stack[this.stack.length - 1]
        heading.data = { ...(heading.data as NodeData), [SINGLE_DASH_FLAG]: true }
      }
    }
  })
  return (tree) => restoreSingleDashParagraphs(tree)
}

function restoreSingleDashParagraphs(node: MdastNode): void {
  for (const child of node.children ?? []) {
    const data = child.data as NodeData
    if (child.type === 'heading' && data?.[SINGLE_DASH_FLAG]) {
      child.type = 'paragraph'
      delete child.depth
      delete data[SINGLE_DASH_FLAG]
      const children = (child.children ??= [])
      const last = children[children.length - 1]
      if (last?.type === 'text') last.value = `${last.value ?? ''}\n-`
      else children.push({ type: 'text', value: '\n-' })
    }
    restoreSingleDashParagraphs(child)
  }
}
