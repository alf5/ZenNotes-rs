// @vitest-environment jsdom

import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_VAULT_SETTINGS, type NoteMeta } from '@shared/ipc'
import { useStore } from '../store'
import type { SearchCreateDraft } from '../lib/search-create'
import { SearchCreateForm } from './SearchCreateForm'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

function note(title: string, tags: string[] = []): NoteMeta {
  return {
    path: `inbox/${title}.md`,
    title,
    folder: 'inbox',
    siblingOrder: 0,
    createdAt: 0,
    updatedAt: 0,
    size: 0,
    tags,
    wikilinks: [],
    hasAttachments: false,
    assetEmbeds: [],
    excerpt: ''
  }
}

const draft: SearchCreateDraft = { name: 'zzqx', folderText: '', tags: [] }

describe('SearchCreateForm: footer hints follow the focused field', () => {
  let host: HTMLDivElement
  let root: Root
  let originalState: ReturnType<typeof useStore.getState>
  let originalScrollIntoView: PropertyDescriptor | undefined

  beforeEach(() => {
    originalState = useStore.getState()
    host = document.createElement('div')
    document.body.append(host)
    root = createRoot(host)
    originalScrollIntoView = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollIntoView')
    Object.defineProperty(Element.prototype, 'scrollIntoView', { configurable: true, value: vi.fn() })

    useStore.setState({
      // The Folder list is built from the system areas rather than from these
      // entries, so an empty `folders` still yields the three picker rows. The
      // tags on this note are what give the Tags field something to list.
      notes: [note('Alpha', ['alpha', 'beta'])],
      folders: [],
      systemFolderLabels: {},
      vaultSettings: DEFAULT_VAULT_SETTINGS,
      activeNote: null
    })
  })

  afterEach(() => {
    act(() => root.unmount())
    host.remove()
    document.body.innerHTML = ''
    if (originalScrollIntoView) {
      Object.defineProperty(Element.prototype, 'scrollIntoView', originalScrollIntoView)
    } else {
      delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView
    }
    vi.restoreAllMocks()
    useStore.setState(originalState, true)
  })

  const render = (): void => {
    act(() =>
      root.render(
        createElement(SearchCreateForm, {
          draft,
          onBack: vi.fn(),
          onCreate: vi.fn(),
          onOpenExisting: vi.fn()
        })
      )
    )
  }

  const field = (id: string): HTMLInputElement => {
    const el = document.querySelector<HTMLInputElement>(`#search-create-${id}`)
    if (!el) throw new Error(`${id} input not rendered`)
    return el
  }

  /** The footer is the only `div` carrying the hook, so this misses the
   *  `Shift+↵` chip on the collision button, which is a `kbd`. */
  const hints = (): string[] =>
    [...document.querySelectorAll('div[data-keyboard-hints] > span')].map(
      (span) => span.textContent?.replace(/\s+/g, ' ').trim() ?? ''
    )

  const focusField = (id: string): void => {
    act(() => {
      field(id).focus()
    })
  }

  const type = (id: string, value: string): void => {
    const el = field(id)
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!
    act(() => {
      setter.call(el, value)
      el.dispatchEvent(new Event('input', { bubbles: true }))
    })
  }

  const press = (id: string, init: KeyboardEventInit): void => {
    act(() => {
      field(id).dispatchEvent(
        new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true })
      )
    })
  }

  it('names Tab and offers create note in Name, with no list keys on arrival', () => {
    render()
    // The form opens focused in Name, which renders no list at all, so the
    // pick hint has nothing to act on and must not be offered.
    expect(hints()).toEqual(['↵ create note', 'tab next field', 'esc back'])
  })

  it('offers pick and create note in Folder, where the system areas always give rows', () => {
    render()
    focusField('folder')
    expect(hints()).toEqual(['↑↓ pick', '↵ create note', 'tab next field', 'esc back'])
  })

  it('names the folder when a row is highlighted', () => {
    render()
    focusField('folder')
    press('folder', { key: 'ArrowDown' })
    expect(hints()).toEqual(['↑↓ pick', '↵ choose folder', 'tab next field', 'esc back'])
  })

  it('drops pick when the typed folder matches nothing', () => {
    render()
    focusField('folder')
    // The system folders guarantee rows only until the text filters them all
    // away; a path that does not exist yet is a legitimate thing to type.
    type('folder', 'zzz')
    expect(hints()).toEqual(['↵ create note', 'tab next field', 'esc back'])
  })

  it('offers pick and create note in Tags once the vault has tags to list', () => {
    render()
    focusField('tags')
    expect(hints()).toEqual(['↑↓ pick', '↵ create note', 'tab next field', 'esc back'])
  })

  it('names the tag when a row is highlighted', () => {
    render()
    focusField('tags')
    press('tags', { key: 'ArrowDown' })
    expect(hints()).toEqual(['↑↓ pick', '↵ add tag', 'tab next field', 'esc back'])
  })

  it('reads the row Enter would pick, not just the index, when the list shrinks under it', () => {
    render()
    // The highlight sits on the second of two vault tags, then the vault loses
    // a tag while the form is open (a sync, another window). The index now
    // outlives its row, so Enter finds nothing behind it and, with nothing
    // typed, creates the note.
    focusField('tags')
    press('tags', { key: 'ArrowDown' })
    press('tags', { key: 'ArrowDown' })
    act(() => {
      useStore.setState({ notes: [note('Alpha', ['alpha'])] })
    })
    expect(hints()).toEqual(['↑↓ pick', '↵ create note', 'tab next field', 'esc back'])
  })

  it('still says add tag once the highlight cycles off the last row', () => {
    render()
    // Two tags in the vault. The highlight wraps from the last row back to the
    // typed value, where Enter still commits what is typed rather than
    // creating the note, so the label must not fall back to `create note`.
    focusField('tags')
    type('tags', 'fresh')
    press('tags', { key: 'ArrowDown' })
    press('tags', { key: 'ArrowDown' })
    press('tags', { key: 'ArrowDown' })
    expect(hints()).toEqual(['↑↓ pick', '↵ add tag', 'tab next field', 'esc back'])
  })

  it('keeps pick when the typed-tag row is all there is, and still says add tag', () => {
    // A vault with no tags: typing leaves exactly one row, the `Add #…` one.
    // Moving the highlight off it does not remove the row, so pick stays true;
    // what changes is that Enter now commits the typed text instead.
    useStore.setState({ notes: [note('Alpha')] })
    render()
    focusField('tags')
    type('tags', 'fresh')
    press('tags', { key: 'ArrowDown' })
    expect(hints()).toEqual(['↑↓ pick', '↵ add tag', 'tab next field', 'esc back'])
  })

  it('offers no Enter hint at all when the typed tag is not a valid tag', () => {
    render()
    focusField('tags')
    // Enter has nothing to do here: no row is highlighted and `normalizeTag`
    // rejects the text, so it neither adds a tag nor creates the note. The
    // reason is already on screen in the status line.
    type('tags', '!!!')
    expect(hints()).toEqual(['tab next field', 'esc back'])
    expect(document.querySelector('[data-search-create-status="error"]')).not.toBeNull()
  })

  it('keeps the hardware-hint hook in every state', () => {
    render()
    expect(document.querySelector('div[data-keyboard-hints]')).not.toBeNull()
    focusField('folder')
    expect(document.querySelector('div[data-keyboard-hints]')).not.toBeNull()
    type('folder', 'zzz')
    expect(document.querySelector('div[data-keyboard-hints]')).not.toBeNull()
    focusField('tags')
    expect(document.querySelector('div[data-keyboard-hints]')).not.toBeNull()
    type('tags', '!!!')
    expect(document.querySelector('div[data-keyboard-hints]')).not.toBeNull()
  })

  it('keeps Back and Create reachable regardless of the hints', () => {
    render()
    // The hook exists so a phone shell can drop the hints while keeping the
    // buttons, so they must survive every state the footer can be in.
    for (const focus of [null, 'folder', 'tags'] as const) {
      if (focus) focusField(focus)
      const labels = [...document.querySelectorAll('button')].map((b) => b.textContent?.trim())
      expect(labels).toContain('Back')
      expect(labels).toContain('Create')
    }
  })
})
