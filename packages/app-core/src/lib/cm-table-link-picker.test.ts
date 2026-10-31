// @vitest-environment jsdom

import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { forceParsing } from '@codemirror/language'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { tablePlugin } from './cm-table'
import {
  cellLinkPick,
  cellLinkQueryAt,
  keepCaretInsideLink,
  startDisplayText
} from './cm-table-link-picker'
import { useStore } from '../store'

describe('cellLinkQueryAt', () => {
  it('finds the unclosed [[query before the caret', () => {
    expect(cellLinkQueryAt('one [[Expr', 10)).toEqual({ open: 4, query: 'Expr', bang: false })
    expect(cellLinkQueryAt('[[', 2)).toEqual({ open: 0, query: '', bang: false })
    expect(cellLinkQueryAt('see ![[log', 10)).toEqual({ open: 5, query: 'log', bang: true })
  })

  it('stays closed once the link is past its note segment', () => {
    expect(cellLinkQueryAt('[[Note]] x', 10)).toBeNull()
    expect(cellLinkQueryAt('[[Note|al', 9)).toBeNull()
    expect(cellLinkQueryAt('[[Note#He', 9)).toBeNull()
    expect(cellLinkQueryAt('[[Note^bl', 9)).toBeNull()
    expect(cellLinkQueryAt('plain text', 10)).toBeNull()
  })

  it('ignores [[ inside an inline code span (#783)', () => {
    expect(cellLinkQueryAt('`[[Expr', 7)).not.toBeNull() // an unclosed backtick is text
    expect(cellLinkQueryAt('`a [[Ex` b', 7)).toBeNull()
    expect(cellLinkQueryAt('``a ` [[Ex`` b', 10)).toBeNull()
    expect(cellLinkQueryAt('`code` [[Ex', 11)).not.toBeNull()
  })
})

describe('cell link edits', () => {
  const note = { kind: 'note' as const, target: 'Expressions' }

  it('closes a link still being typed and lands past it', () => {
    const match = cellLinkQueryAt('one [[Expr', 10)!
    expect(cellLinkPick('one [[Expr', 10, match, note)).toEqual({
      text: 'one [[Expressions]]',
      from: 19,
      to: 19
    })
  })

  it('replaces only the note segment of a closed link (#686)', () => {
    const text = '[[Exp#Intro|alias]] after'
    const match = cellLinkQueryAt(text, 5)!
    expect(cellLinkPick(text, 5, match, note).text).toBe('[[Expressions#Intro|alias]] after')
  })

  it('makes an asset an embed, once', () => {
    const asset = { kind: 'asset' as const, target: 'img/logo.png' }
    const plain = cellLinkQueryAt('x [[log', 7)!
    expect(cellLinkPick('x [[log', 7, plain, asset).text).toBe('x ![[img/logo.png]]')
    const bang = cellLinkQueryAt('x ![[log', 8)!
    expect(cellLinkPick('x ![[log', 8, bang, asset).text).toBe('x ![[img/logo.png]]')
  })

  it('Tab keeps the caret inside the brackets', () => {
    const match = cellLinkQueryAt('[[Ex', 4)!
    expect(keepCaretInsideLink(cellLinkPick('[[Ex', 4, match, note))).toEqual({
      text: '[[Expressions]]',
      from: 13,
      to: 13
    })
  })

  it('| starts the display text, or selects the one already there (#804)', () => {
    const fresh = cellLinkQueryAt('[[Ex', 4)!
    expect(startDisplayText(cellLinkPick('[[Ex', 4, fresh, note))).toEqual({
      text: '[[Expressions|]]',
      from: 14,
      to: 14
    })
    const aliased = cellLinkQueryAt('[[Ex|shown]]', 4)
    expect(aliased).not.toBeNull()
    const edit = startDisplayText(cellLinkPick('[[Ex|shown]]', 4, aliased!, note))
    expect(edit.text.slice(edit.from, edit.to)).toBe('shown')
  })
})

function noteMeta(path: string, title: string): unknown {
  return {
    path,
    title,
    folder: 'inbox',
    siblingOrder: 0,
    createdAt: 0,
    updatedAt: 0,
    size: 0,
    tags: [],
    wikilinks: [],
    hasAttachments: false,
    excerpt: ''
  }
}

describe('[[ in a table cell (#977)', () => {
  const saved = {
    notes: useStore.getState().notes,
    assetFiles: useStore.getState().assetFiles,
    folders: useStore.getState().folders,
    activeNote: useStore.getState().activeNote,
    vimMode: useStore.getState().vimMode
  }
  beforeEach(() => {
    useStore.setState({
      notes: [
        noteMeta('inbox/Expressions.md', 'Expressions'),
        noteMeta('inbox/Expert Advice.md', 'Expert Advice'),
        noteMeta('inbox/Table.md', 'Table')
      ],
      assetFiles: [],
      folders: [],
      activeNote: null
    } as never)
  })
  afterEach(() => {
    useStore.setState(saved as never)
    document.querySelectorAll('.cm-tooltip-autocomplete').forEach((el) => el.parentElement?.remove())
  })

  const DOC = 'Intro.\n\n| A | B |\n| --- | --- |\n| one | two |\n\nAfter.'

  function mount(): EditorView {
    const parent = document.createElement('div')
    document.body.append(parent)
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: DOC,
        extensions: [markdown({ base: markdownLanguage }), tablePlugin]
      })
    })
    forceParsing(view, DOC.length, 5000)
    return view
  }

  /** Type into the focused cell the way the browser does: new text, caret at
   *  its end, then an input event. */
  function typeInto(cell: HTMLElement, text: string): void {
    cell.textContent = text
    const range = document.createRange()
    range.setStart(cell.firstChild!, text.length)
    range.collapse(true)
    window.getSelection()?.removeAllRanges()
    window.getSelection()?.addRange(range)
    cell.dispatchEvent(new Event('input', { bubbles: true }))
  }

  function key(cell: HTMLElement, init: KeyboardEventInit): KeyboardEvent {
    const event = new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init })
    cell.dispatchEvent(event)
    return event
  }

  const popup = (): HTMLElement | null => document.querySelector('.cm-tooltip-autocomplete')
  const options = (): string[] =>
    [...document.querySelectorAll('.cm-tooltip-autocomplete li')].map(
      (li) => li.querySelector('.wikilink-cmd-label')?.textContent ?? ''
    )
  const selected = (): string | null | undefined =>
    document.querySelector('.cm-tooltip-autocomplete li[aria-selected] .wikilink-cmd-label')
      ?.textContent

  it('opens the note picker and inserts the picked link (Vim off)', () => {
    useStore.setState({ vimMode: false })
    const view = mount()
    const cell = view.dom.querySelector<HTMLElement>('.cm-table-cell[data-row="0"][data-col="0"]')!
    cell.focus()
    typeInto(cell, 'one [[Exp')
    expect(popup()).toBeTruthy()
    expect(options()).toEqual(expect.arrayContaining(['Expressions', 'Expert Advice']))
    expect(options()).not.toContain('Table')

    // The arrows move the highlight instead of leaving the cell.
    const first = selected()
    expect(key(cell, { key: 'ArrowDown' }).defaultPrevented).toBe(true)
    expect(selected()).not.toBe(first)
    key(cell, { key: 'ArrowUp' })
    expect(selected()).toBe(first)

    expect(key(cell, { key: 'Enter' }).defaultPrevented).toBe(true)
    expect(popup()).toBeNull()
    expect(cell.dataset.raw).toBe(`one [[${first}]]`)
    expect(document.activeElement).toBe(cell)

    view.focus()
    expect(view.state.doc.toString()).toContain(`| one [[${first}]] |`)
    view.destroy()
  })

  it('Escape closes the picker without leaving the cell', () => {
    useStore.setState({ vimMode: false })
    const view = mount()
    const cell = view.dom.querySelector<HTMLElement>('.cm-table-cell[data-row="0"][data-col="1"]')!
    cell.focus()
    typeInto(cell, '[[Expr')
    expect(popup()).toBeTruthy()
    expect(key(cell, { key: 'Escape' }).defaultPrevented).toBe(true)
    expect(popup()).toBeNull()
    expect(document.activeElement).toBe(cell)
    expect(cell.dataset.raw).toBe('[[Expr')
    view.destroy()
  })

  it('closes when the cell loses focus or the query stops matching', () => {
    useStore.setState({ vimMode: false })
    const view = mount()
    const cell = view.dom.querySelector<HTMLElement>('.cm-table-cell[data-row="0"][data-col="0"]')!
    cell.focus()
    typeInto(cell, '[[zzzz')
    expect(popup()).toBeNull()
    typeInto(cell, '[[Ex')
    expect(popup()).toBeTruthy()
    cell.blur()
    expect(popup()).toBeNull()
    view.destroy()
  })

  it('works in Vim INSERT, where the first Escape only closes the picker', () => {
    useStore.setState({ vimMode: true })
    const view = mount()
    const cell = view.dom.querySelector<HTMLElement>('.cm-table-cell[data-row="0"][data-col="0"]')!
    cell.focus()
    key(cell, { key: 'A' })
    expect(cell.getAttribute('contenteditable')).toBe('true')
    typeInto(cell, 'one [[Expr')
    expect(options()).toEqual(['Expressions'])
    key(cell, { key: 'Escape' })
    expect(popup()).toBeNull()
    expect(cell.getAttribute('contenteditable')).toBe('true')

    typeInto(cell, 'one [[Expr')
    expect(popup()).toBeTruthy()
    key(cell, { key: 'Tab' })
    expect(cell.dataset.raw).toBe('one [[Expressions]]')
    expect(cell.getAttribute('contenteditable')).toBe('true')
    key(cell, { key: 'Escape' })
    expect(cell.getAttribute('contenteditable')).toBe('false')
    view.destroy()
  })
})
