// @vitest-environment jsdom

import { act, createElement } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { PromptModal, shouldAutofocusPrompt, type PromptOptions } from './PromptModal'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

// Focusing the input pops the soft keyboard, which on a phone covers the very
// suggestion list the prompt is asking the user to choose from (the folder
// picker in Move to… was unusable one-handed). Touch + a list = tap-first.
describe('shouldAutofocusPrompt', () => {
  it('does not autofocus a touch prompt that has suggestions (folder pickers)', () => {
    expect(shouldAutofocusPrompt(true, 1)).toBe(false)
    expect(shouldAutofocusPrompt(true, 12)).toBe(false)
  })

  it('autofocuses a touch prompt with no list — those are pure typing', () => {
    // Rename note / New folder: nothing to tap, so the keyboard is the point.
    expect(shouldAutofocusPrompt(true, 0)).toBe(true)
  })

  it('always autofocuses with a fine pointer, so desktop is unchanged', () => {
    expect(shouldAutofocusPrompt(false, 0)).toBe(true)
    expect(shouldAutofocusPrompt(false, 8)).toBe(true)
  })
})

// The rule above held only inside PromptModal: the dialog shell around it
// focused the panel's first control, the input, on every open, so from 2.52.0
// the folder picker raised the keyboard on phones anyway. These render the
// prompt in its real shell and look at where focus ends up.
describe('PromptModal focus inside the dialog shell', () => {
  const folderPicker: PromptOptions = {
    title: 'Move "Drafts" to…',
    placeholder: 'Inbox (type a folder to change)',
    suggestions: [{ value: 'Work' }, { value: 'Work/Research' }]
  }

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    document.body.innerHTML = ''
  })

  it('keeps a touch folder picker tap-first: focus on the panel, not the input', () => {
    pointer('coarse')
    const editor = focusedEditor()

    open(folderPicker)

    const panel = document.querySelector('[data-prompt-modal] [role="dialog"]')
    expect(document.activeElement).toBe(panel)
    expect(document.activeElement).not.toBe(editor)
    expect(document.activeElement?.tagName).not.toBe('INPUT')
  })

  it('still focuses the input of a touch prompt with nothing to tap', () => {
    pointer('coarse')
    open({ title: 'New folder', placeholder: 'Folder name' })

    expect(document.activeElement).toBe(input())
  })

  it('focuses and selects the input with a fine pointer, list or not', () => {
    pointer('fine')
    open({ ...folderPicker, initialValue: 'Wo' })

    expect(document.activeElement).toBe(input())
    expect(input().selectionStart).toBe(0)
    expect(input().selectionEnd).toBe(2)
  })
})

function pointer(kind: 'coarse' | 'fine'): void {
  vi.stubGlobal(
    'matchMedia',
    (query: string) =>
      ({
        matches: query === `(pointer: ${kind})`,
        media: query,
        addEventListener: () => undefined,
        removeEventListener: () => undefined
      }) as unknown as MediaQueryList
  )
}

function focusedEditor(): HTMLTextAreaElement {
  const editor = document.createElement('textarea')
  document.body.append(editor)
  editor.focus()
  return editor
}

function input(): HTMLInputElement {
  return document.querySelector('[data-prompt-modal] input') as HTMLInputElement
}

// The prompt's own autofocus runs on a zero timeout, after the shell's.
function open(options: PromptOptions): void {
  vi.useFakeTimers()
  const host = document.createElement('div')
  document.body.append(host)
  const root = createRoot(host)
  act(() => {
    root.render(createElement(PromptModal, { options, onSubmit: vi.fn(), onCancel: vi.fn() }))
  })
  act(() => {
    vi.runAllTimers()
  })
}
