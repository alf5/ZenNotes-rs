// @vitest-environment jsdom
//
// Vim `o` / `O` pressed for real, mounted like EditorPane's editor (note
// grammar, Vim-aware markdown Enter, ordered-list renumbering): on a line in a
// quote or callout the new line keeps the prefix Enter would give it.
import { afterEach, describe, expect, it } from 'vitest'
import { EditorState } from '@codemirror/state'
import { EditorView, keymap } from '@codemirror/view'
import { vim } from '@replit/codemirror-vim'
import { noteMarkdown } from './cm-markdown-language'
import { orderedListRenumber } from './cm-ordered-list-renumber'
import { vimAwareDefaultKeymap, vimAwareMarkdownKeymap } from './cm-vim-default-keymap'
import { quotedOpenLinePrefix, registerOpenLineContinuation } from './cm-vim-open-line'

const views: EditorView[] = []
afterEach(() => views.splice(0).forEach((v) => v.destroy()))

/** `doc` with the caret at the start of the first line equal to `lineText`. */
function mount(doc: string, lineText?: string): EditorView {
  registerOpenLineContinuation()
  const lines = doc.split('\n')
  const at = lineText === undefined ? 0 : lines.indexOf(lineText)
  if (at < 0) throw new Error(`no line ${JSON.stringify(lineText)}`)
  const anchor = lines.slice(0, at).reduce((n, line) => n + line.length + 1, 0)
  const view = new EditorView({
    state: EditorState.create({
      doc,
      selection: { anchor },
      extensions: [
        vim(),
        noteMarkdown(),
        vimAwareMarkdownKeymap,
        orderedListRenumber,
        keymap.of([...vimAwareDefaultKeymap(true)])
      ]
    }),
    parent: document.body
  })
  views.push(view)
  view.focus()
  return view
}

function press(view: EditorView, ...keys: string[]): void {
  for (const key of keys) {
    view.contentDOM.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }))
  }
}

function typeInput(view: EditorView, text: string): void {
  const pos = view.state.selection.main.head
  view.dispatch({
    changes: { from: pos, insert: text },
    selection: { anchor: pos + text.length },
    userEvent: 'input.type'
  })
}

/** Press `keys` in normal mode, type `zz`, leave insert mode. */
function openLine(doc: string, lineText: string, ...keys: string[]): string {
  const view = mount(doc, lineText)
  press(view, ...keys)
  typeInput(view, 'zz')
  press(view, 'Escape')
  return view.state.doc.toString()
}

const prefixOf = (doc: string, lineText: string): string | null => {
  const view = mount(doc, lineText)
  return quotedOpenLinePrefix(view.state, view.state.doc.lineAt(view.state.selection.main.head).number)
}

describe('quotedOpenLinePrefix', () => {
  it('is what Enter continues a quoted line with', () => {
    expect(prefixOf('> hello', '> hello')).toBe('> ')
    expect(prefixOf('> > deep', '> > deep')).toBe('> > ')
    expect(prefixOf('> [!tip] Title\n> body', '> [!tip] Title')).toBe('> ')
    expect(prefixOf('> - item', '> - item')).toBe('> - ')
    expect(prefixOf('> * [x] done', '> * [x] done')).toBe('> * [ ] ')
    expect(prefixOf('> 1. one', '> 1. one')).toBe('> 2. ')
    expect(prefixOf('> - item\n>   more', '>   more')).toBe('>   ')
    expect(prefixOf('- > quoted in a list', '- > quoted in a list')).toBe('  > ')
    expect(prefixOf('> hello\nlazy', 'lazy')).toBe('> ')
  })

  it('follows the quoted-fence rule on code lines (#961)', () => {
    const fence = '> ```js\n> if (a) {\n>     b()\n> ```'
    expect(prefixOf(fence, '> ```js')).toBe('> ')
    expect(prefixOf(fence, '> if (a) {')).toBe('> ')
    expect(prefixOf(fence, '>     b()')).toBe('>     ')
    expect(prefixOf(fence, '> ```')).toBe('> ')
    expect(prefixOf('> - ```js\n>   xy\n>   ```', '>   xy')).toBe('>   ')
  })

  it('continues the markup where Enter would end it', () => {
    // Enter on an empty item drops its marker; `o` opens another item, as it
    // does on an empty item outside a quote.
    expect(prefixOf('> - ', '> - ')).toBe('> - ')
    expect(prefixOf('> 1. a\n> 2. ', '> 2. ')).toBe('> 3. ')
    // Enter on a second blank `>` line leaves the quote.
    expect(prefixOf('> a\n>\n>', '>')).toBe('> ')
    const view = mount('> a\n>\n>')
    expect(quotedOpenLinePrefix(view.state, 3)).toBe('> ')
  })

  it('is null outside a quote', () => {
    expect(prefixOf('plain', 'plain')).toBeNull()
    expect(prefixOf('- item', '- item')).toBeNull()
    expect(prefixOf('```\n> prompt\n```', '> prompt')).toBeNull()
    expect(prefixOf('> a\n\nafter', 'after')).toBeNull()
    expect(prefixOf('> a\n', '')).toBeNull()
  })
})

describe('Vim o / O inside a quote or callout', () => {
  it('opens a quoted line below and above', () => {
    expect(openLine('> hello', '> hello', 'o')).toBe('> hello\n> zz')
    expect(openLine('> hello', '> hello', 'O')).toBe('> zz\n> hello')
    expect(openLine('> > deep', '> > deep', 'o')).toBe('> > deep\n> > zz')
  })

  it('stays in a callout', () => {
    const callout = '> [!tip] Title\n> body\n\nafter'
    expect(openLine(callout, '> body', 'o')).toBe('> [!tip] Title\n> body\n> zz\n\nafter')
    expect(openLine(callout, '> [!tip] Title', 'o')).toBe('> [!tip] Title\n> zz\n> body\n\nafter')
  })

  it('continues a list inside a quote', () => {
    expect(openLine('> - item', '> - item', 'o')).toBe('> - item\n> - zz')
    expect(openLine('> - item', '> - item', 'O')).toBe('> - zz\n> - item')
    expect(openLine('> - [x] done', '> - [x] done', 'o')).toBe('> - [x] done\n> - [ ] zz')
    expect(openLine('> - item\n>   more', '>   more', 'o')).toBe('> - item\n>   more\n>   zz')
  })

  it('renumbers an ordered list inside a quote', () => {
    expect(openLine('> 1. one\n> 2. two', '> 1. one', 'o')).toBe('> 1. one\n> 2. zz\n> 3. two')
    expect(openLine('> 1. one\n> 2. two', '> 2. two', 'O')).toBe('> 1. one\n> 2. zz\n> 3. two')
  })

  it('keeps a quoted code block whole', () => {
    const fence = '> [!tip] Code\n> ```js\n> xy\n> ```'
    expect(openLine(fence, '> xy', 'o')).toBe('> [!tip] Code\n> ```js\n> xy\n> zz\n> ```')
    expect(openLine(fence, '> xy', 'O')).toBe('> [!tip] Code\n> ```js\n> zz\n> xy\n> ```')
  })

  it('repeats with a count and with .', () => {
    expect(openLine('> hello', '> hello', '2', 'o')).toBe('> hello\n> zz\n> zz')
    const view = mount('> hello', '> hello')
    press(view, 'o')
    typeInput(view, 'zz')
    press(view, 'Escape', '.')
    expect(view.state.doc.toString()).toBe('> hello\n> zz\n> zz')
  })

  it('leaves the current line as it is', () => {
    expect(openLine('> - a\n> - \n', '> - ', 'o')).toBe('> - a\n> - \n> - zz\n')
    expect(openLine('> a\n>\n> b', '>', 'o')).toBe('> a\n>\n> zz\n> b')
  })
})

describe('Vim o / O outside a quote (unchanged)', () => {
  it('continues a list (#320) and opens a plain line elsewhere', () => {
    expect(openLine('- item', '- item', 'o')).toBe('- item\n- zz')
    expect(openLine('1. one\n2. two', '1. one', 'o')).toBe('1. one\n2. zz\n3. two')
    expect(openLine('plain', 'plain', 'o')).toBe('plain\nzz')
    expect(openLine('plain', 'plain', 'O')).toBe('zz\nplain')
    expect(openLine('```\n> prompt\n```', '> prompt', 'o')).toBe('```\n> prompt\nzz\n```')
  })
})
