// @vitest-environment jsdom
// #405: fenced code inside a bullet list must indent on Enter instead of
// escaping to column 0. Exercises the real dispatch chain both editors use:
// vim + markdown keymap (with our fence-indent Enter) + the block snippet.
import { afterEach, describe, expect, it } from 'vitest'
import { EditorState } from '@codemirror/state'
import { EditorView, keymap } from '@codemirror/view'
import { vim } from '@replit/codemirror-vim'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { vimAwareDefaultKeymap, vimAwareMarkdownKeymap } from './cm-vim-default-keymap'
import { markdownSnippetExtension } from './cm-markdown-snippets'
import { noteMarkdown } from './cm-markdown-language'

const views: EditorView[] = []
afterEach(() => views.splice(0).forEach((v) => v.destroy()))

type MountOptions = {
  withSnippets?: boolean
  // EditorPane's note grammar, which parses a ```js body as JavaScript.
  note?: boolean
  vimMode?: boolean
  cursor?: number
}

function mount(
  doc: string,
  { withSnippets = true, note = false, vimMode = true, cursor }: MountOptions = {}
): EditorView {
  const view = new EditorView({
    state: EditorState.create({
      doc,
      selection: cursor === undefined ? undefined : { anchor: cursor },
      // In EditorPane's order: the snippets ahead of the markdown keymap, both
      // Prec.high, so `$$` + Enter auto-closes before the markdown command can
      // continue a list or quote (#961).
      extensions: [
        ...(withSnippets ? [markdownSnippetExtension()] : []),
        ...(vimMode ? [vim()] : []),
        note ? noteMarkdown() : markdown({ base: markdownLanguage, addKeymap: false }),
        vimAwareMarkdownKeymap,
        keymap.of([...vimAwareDefaultKeymap(vimMode)])
      ]
    }),
    parent: document.body
  })
  views.push(view)
  view.focus()
  return view
}

const press = (view: EditorView, key: string, keyCode: number): void => {
  view.contentDOM.dispatchEvent(
    new KeyboardEvent('keydown', { key, keyCode, bubbles: true, cancelable: true })
  )
}

// Insert text as a user input event so the block snippet registers a pending
// opener, exactly as typing would.
function typeInput(view: EditorView, text: string): void {
  const pos = view.state.selection.main.head
  view.dispatch({
    changes: { from: pos, insert: text },
    selection: { anchor: pos + text.length },
    userEvent: 'input.type'
  })
}

describe('fenced code Enter indentation in lists (#405)', () => {
  it('auto-closes a fence typed in a bullet item, indented to the fence column', () => {
    const view = mount('- ')
    press(view, 'A', 65) // append → insert mode at end of line
    typeInput(view, '```bash')
    press(view, 'Enter', 13)
    // The snippet owns this (pending): content line + closing fence, both at col 2.
    expect(view.state.doc.toString()).toBe('- ```bash\n  \n  ```')
  })

  it('indents an existing fence opener when the snippet is not pending (fallback)', () => {
    const view = mount('- ```bash') // nothing typed → no pending snippet
    press(view, 'A', 65) // insert mode at end of the fence line
    press(view, 'Enter', 13)
    // Our Enter command indents the new line into the list item (no auto-close).
    expect(view.state.doc.toString()).toBe('- ```bash\n  ')
  })

  it('continues indented content lines inside the block', () => {
    const view = mount('- ```bash\n  echo hi')
    view.dispatch({ selection: { anchor: 19 } }) // end of "  echo hi"
    press(view, 'i', 73) // insert mode
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('- ```bash\n  echo hi\n  ')
  })

  it('leaves a top-level fence at column 0', () => {
    const view = mount('')
    press(view, 'A', 65)
    typeInput(view, '```bash')
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('```bash\n\n```')
  })

  it('does not edit on Enter in Vim normal mode (it is the <CR> motion)', () => {
    const view = mount('- ```bash')
    view.dispatch({ selection: { anchor: 9 } }) // stay in normal mode
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('- ```bash')
  })
})

// #961: in a quote or callout the quote marker carries onto the new lines.
describe('block Enter inside quotes and callouts (#961)', () => {
  it('auto-closes a math block typed in a callout, every line quoted', () => {
    const view = mount('> [!note] Note heading\n> ')
    press(view, 'G', 71) // last line
    press(view, 'A', 65)
    typeInput(view, '$$')
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> [!note] Note heading\n> $$\n> \n> $$')
  })

  it('auto-closes a fence typed in a quote, every line quoted', () => {
    const view = mount('> ')
    press(view, 'A', 65)
    typeInput(view, '```js')
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```js\n> \n> ```')
  })

  it('keeps the quote on Enter at an existing quoted fence opener (fallback)', () => {
    const view = mount('> ```js')
    press(view, 'A', 65)
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```js\n> ')
  })

  it('keeps the quote and turns the list marker to spaces for a fence in a quoted item', () => {
    const view = mount('> - ```js')
    press(view, 'A', 65)
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> - ```js\n>   ')
  })
})

// lang-markdown's Enter gives up inside FencedCode and the default newline
// copies only leading whitespace, so Enter on a code line in a quote or callout
// dropped the `>`: the new line left the callout and broke the block. These
// mount the note grammar, as EditorPane does, so a ```js body is JavaScript.
describe('Enter on a fenced code line inside a quote or callout', () => {
  const CALLOUT = '> [!tip] Code\n> ```js\n> xy\n> ```'
  const endOf = (doc: string, text: string): number => doc.indexOf(text) + text.length

  it('keeps the callout on a content line in insert mode', () => {
    const view = mount(CALLOUT, { note: true, cursor: CALLOUT.indexOf('> xy') })
    press(view, 'A', 65)
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> [!tip] Code\n> ```js\n> xy\n> \n> ```')
    expect(view.state.selection.main.head).toBe(endOf(CALLOUT, '> xy') + 3)
  })

  it('keeps the callout with Vim off', () => {
    const view = mount(CALLOUT, { note: true, vimMode: false, cursor: endOf(CALLOUT, '> xy') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> [!tip] Code\n> ```js\n> xy\n> \n> ```')
  })

  it("carries the line's own indentation after the quote", () => {
    const doc = '> ```js\n> if (a) {\n>     b()\n> ```'
    const view = mount(doc, { note: true, vimMode: false, cursor: endOf(doc, 'b()') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```js\n> if (a) {\n>     b()\n>     \n> ```')
  })

  it('splits a line, the rest landing after the quote', () => {
    const doc = '> ```js\n> foo bar\n> ```'
    const view = mount(doc, { note: true, vimMode: false, cursor: endOf(doc, 'foo') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```js\n> foo\n> bar\n> ```')
  })

  it('reads a `>` that starts the code as code, not as a quote marker', () => {
    const doc = '> ```py\n> >>> 1 + 1\n> ```'
    const view = mount(doc, { note: true, vimMode: false, cursor: endOf(doc, '1 + 1') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```py\n> >>> 1 + 1\n> \n> ```')
  })

  it('stays in the callout after the closing fence', () => {
    const view = mount(CALLOUT, { note: true, vimMode: false, cursor: CALLOUT.length })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe(`${CALLOUT}\n> `)
  })

  it('keeps both markers in a nested quote', () => {
    const doc = '> > ```js\n> > xy\n> > ```'
    const view = mount(doc, { note: true, vimMode: false, cursor: endOf(doc, 'xy') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> > ```js\n> > xy\n> > \n> > ```')
  })

  it('keeps the quote and the item indent for a fence in a quoted list item', () => {
    const doc = '> - ```js\n>   xy\n>   ```'
    const view = mount(doc, { note: true, vimMode: false, cursor: endOf(doc, 'xy') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> - ```js\n>   xy\n>   \n>   ```')
  })

  it('trims the blank quoted line it leaves behind', () => {
    const doc = '> ```js\n> xy\n> '
    const view = mount(doc, { note: true, vimMode: false, cursor: doc.length })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```js\n> xy\n>\n> ')
  })

  it('leaves a line with no `>` after a quoted fence alone (it ends the quote)', () => {
    const doc = '> ```js\n> xy\nzz'
    const view = mount(doc, { note: true, vimMode: false, cursor: doc.length })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('> ```js\n> xy\nzz\n')
  })

  it('leaves an unquoted fence to the language indent', () => {
    const doc = '```js\nf() {}\n```'
    const view = mount(doc, { note: true, vimMode: false, cursor: endOf(doc, '{') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe('```js\nf() {\n  \n}\n```')
  })

  it('does not edit on Enter in Vim normal mode (it is the <CR> motion)', () => {
    const view = mount(CALLOUT, { note: true, cursor: CALLOUT.indexOf('> xy') })
    press(view, 'Enter', 13)
    expect(view.state.doc.toString()).toBe(CALLOUT)
  })
})
