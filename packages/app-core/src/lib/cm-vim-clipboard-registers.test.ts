// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { EditorState, type Extension } from '@codemirror/state'
import { EditorView } from '@codemirror/view'

// Stand-ins for codemirror-vim: the overridden actions land in `actions`, and
// registers keep the text and linewise flag the overrides give them.
type Action = (this: unknown, cm: unknown, actionArgs: Record<string, unknown>, vim: unknown) => void
const actions: Record<string, Action> = {}
function makeRegister(text = '') {
  return {
    text,
    linewise: false,
    toString() { return this.text },
    setText(next: string, linewise?: boolean) { this.text = next; this.linewise = !!linewise }
  }
}
const registers: Record<string, ReturnType<typeof makeRegister>> = {}
const unnamedRegister = makeRegister()
const controller = {
  pushText: vi.fn(),
  unnamedRegister,
  getRegister: (name?: string) => (name ? (registers[name] ??= makeRegister()) : unnamedRegister)
}
const exitVisualMode = vi.fn()
const handleKey = vi.fn()
const getCM = vi.fn()

vi.mock('@replit/codemirror-vim', () => ({
  Vim: {
    getRegisterController: () => controller,
    defineAction: (name: string, fn: Action) => { actions[name] = fn },
    exitVisualMode,
    handleKey
  },
  getCM
}))

const { editorImagePaste } = await import('./editor-paste-images')
const { installSystemClipboardRegisters, setPasteFromClipboardEnabled, vimClipboardPasteExtension } =
  await import('./cm-vim-clipboard')

function stubClipboard(parts: Record<string, BlobPart> | null): void {
  const items = parts
    ? [{ types: Object.keys(parts), getType: async (type: string) => new Blob([parts[type]], { type }) }]
    : []
  Object.defineProperty(navigator, 'clipboard', {
    value: { read: vi.fn().mockResolvedValue(items), readText: vi.fn().mockResolvedValue('') },
    configurable: true
  })
}

let views: EditorView[] = []
function editor(doc: string, cursor: number, withImageImport = true, extra: Extension[] = []) {
  const importImages = vi.fn()
  let headAtImport = -1
  importImages.mockImplementation((_files: File[], view: EditorView) => {
    headAtImport = view.state.selection.main.head
  })
  const view = new EditorView({
    state: EditorState.create({
      doc,
      selection: { anchor: cursor },
      extensions: [...(withImageImport ? [editorImagePaste.of(importImages)] : []), ...extra]
    })
  })
  views.push(view)
  const cm = { cm6: view, state: { vim: { visualMode: false, insertMode: false } }, replaceSelection: vi.fn() }
  return { view, cm, importImages, headAt: () => headAtImport }
}
const vimActions = { continuePaste: vi.fn() }
const run = (name: string, cm: unknown, actionArgs: Record<string, unknown>) =>
  actions[name].call(vimActions, cm, actionArgs, {})

beforeEach(() => installSystemClipboardRegisters())
afterEach(() => {
  for (const view of views) view.destroy()
  views = []
  vimActions.continuePaste.mockClear()
  exitVisualMode.mockClear()
  handleKey.mockClear()
  for (const name of Object.keys(registers)) delete registers[name]
  unnamedRegister.text = ''
  setPasteFromClipboardEnabled(false)
})

describe('Vim "+ and "* puts', () => {
  it('"+p puts a clipboard image on its own line below the cursor line', async () => {
    stubClipboard({ 'image/png': 'png' })
    const { cm, importImages, headAt } = editor('first line\nsecond', 3)

    run('paste', cm, { registerName: '+', after: true })

    await vi.waitFor(() => expect(importImages).toHaveBeenCalledTimes(1))
    const [files] = importImages.mock.calls[0]
    expect(files[0].type).toBe('image/png')
    expect(headAt()).toBe('first line'.length)
    expect(vimActions.continuePaste).not.toHaveBeenCalled()
  })

  it('"*P puts the image above the cursor line, leaving visual mode first', async () => {
    stubClipboard({ 'image/png': 'png' })
    const { cm, importImages, headAt } = editor('first line\nsecond', 14)
    cm.state.vim.visualMode = true

    run('paste', cm, { registerName: '*', after: false })

    await vi.waitFor(() => expect(importImages).toHaveBeenCalledTimes(1))
    expect(headAt()).toBe('first line\n'.length)
    expect(exitVisualMode).toHaveBeenCalledWith(cm)
  })

  it('"*p pastes clipboard text through Vim, linewise when it ends in a newline', async () => {
    stubClipboard({ 'text/plain': 'a whole line\n' })
    const { cm, importImages } = editor('first line', 0)

    run('paste', cm, { registerName: '*', after: true })

    await vi.waitFor(() => expect(vimActions.continuePaste).toHaveBeenCalledTimes(1))
    const [, , , text, register] = vimActions.continuePaste.mock.calls[0]
    expect(text).toBe('a whole line\n')
    expect(register.linewise).toBe(true)
    expect(importImages).not.toHaveBeenCalled()
  })

  it('an editor without an image import pastes the clipboard text instead', async () => {
    stubClipboard({ 'image/png': 'png', 'text/plain': 'alt text' })
    const { cm } = editor('first line', 0, false)
    cm.state.vim.visualMode = true

    run('paste', cm, { registerName: '+', after: true })

    await vi.waitFor(() => expect(vimActions.continuePaste).toHaveBeenCalledTimes(1))
    expect(vimActions.continuePaste.mock.calls[0][3]).toBe('alt text')
    expect(exitVisualMode).not.toHaveBeenCalled()
  })

  it('other registers paste their own text without reading the clipboard', () => {
    stubClipboard({ 'image/png': 'png' })
    registers.a = makeRegister('named')
    const { cm, importImages } = editor('first line', 0)

    run('paste', cm, { registerName: 'a', after: true })

    expect(vimActions.continuePaste).toHaveBeenCalledWith(cm, { registerName: 'a', after: true }, {}, 'named', registers.a)
    expect(navigator.clipboard.read).not.toHaveBeenCalled()
    expect(importImages).not.toHaveBeenCalled()
  })
})

describe('insert-mode <C-r>+ and <C-r>*', () => {
  it('<C-r>+ inserts a clipboard image at the cursor', async () => {
    stubClipboard({ 'image/png': 'png' })
    const { cm, importImages, headAt } = editor('first line', 5)

    run('insertRegister', cm, { selectedCharacter: '+' })

    await vi.waitFor(() => expect(importImages).toHaveBeenCalledTimes(1))
    expect(headAt()).toBe(5)
    expect(cm.replaceSelection).not.toHaveBeenCalled()
  })

  it('<C-r>* inserts clipboard text', async () => {
    stubClipboard({ 'text/plain': 'from the clipboard' })
    const { cm } = editor('first line', 5)

    run('insertRegister', cm, { selectedCharacter: '*' })

    await vi.waitFor(() => expect(cm.replaceSelection).toHaveBeenCalledWith('from the clipboard'))
  })

  it('<C-r>a still inserts the named register', () => {
    stubClipboard({ 'text/plain': 'clipboard' })
    registers.a = makeRegister('named')
    const { cm } = editor('first line', 5)

    run('insertRegister', cm, { selectedCharacter: 'a' })

    expect(cm.replaceSelection).toHaveBeenCalledWith('named')
    expect(navigator.clipboard.read).not.toHaveBeenCalled()
  })
})

describe('p with yank-to-clipboard on', () => {
  it('puts a clipboard image instead of replaying Vim\'s p', async () => {
    stubClipboard({ 'image/png': 'png' })
    const { view, cm, importImages, headAt } = editor('first line\nsecond', 2, true, [vimClipboardPasteExtension])
    getCM.mockReturnValue(cm)
    setPasteFromClipboardEnabled(true)

    view.contentDOM.dispatchEvent(new KeyboardEvent('keydown', { key: 'p', bubbles: true, cancelable: true }))

    await vi.waitFor(() => expect(importImages).toHaveBeenCalledTimes(1))
    expect(headAt()).toBe('first line'.length)
    expect(handleKey).not.toHaveBeenCalled()
  })

  it('loads clipboard text into the unnamed register and replays p', async () => {
    stubClipboard({ 'text/plain': 'pasted' })
    const { view, cm } = editor('first line', 0, true, [vimClipboardPasteExtension])
    getCM.mockReturnValue(cm)
    setPasteFromClipboardEnabled(true)

    view.contentDOM.dispatchEvent(new KeyboardEvent('keydown', { key: 'p', bubbles: true, cancelable: true }))

    await vi.waitFor(() => expect(handleKey).toHaveBeenCalledWith(cm, 'p', 'user'))
    expect(unnamedRegister.text).toBe('pasted')
  })
})
