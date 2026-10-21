// @vitest-environment jsdom

import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Modal } from '../components/ui/Modal'
import {
  readSavedKeymapOverrides,
  resolveCloseShortcut,
  type CloseShortcutState
} from './close-shortcut'
import { UNBOUND_BINDING } from './keymaps'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

function setPlatform(platform: NodeJS.Platform): void {
  Object.defineProperty(window, 'zen', {
    configurable: true,
    value: { platformSync: () => platform }
  })
}

function keydown(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init })
}

const cmdW = (): KeyboardEvent => keydown({ key: 'w', code: 'KeyW', metaKey: true })
const ctrlW = (): KeyboardEvent => keydown({ key: 'w', code: 'KeyW', ctrlKey: true })

function state(overrides: Partial<CloseShortcutState> = {}): CloseShortcutState {
  return {
    keymapOverrides: {},
    vimMode: true,
    selectedPath: 'Inbox/Note.md',
    settingsOpen: false,
    searchOpen: false,
    vaultTextSearchOpen: false,
    commandPaletteOpen: false,
    bufferPaletteOpen: false,
    templatePaletteOpen: false,
    embedDrawingPaletteOpen: false,
    outlinePaletteOpen: false,
    ...overrides
  }
}

/** The Settings panel as SettingsModal draws it (pinned by SettingsModal.test). */
function showSettingsPanel(): void {
  const panel = document.createElement('div')
  panel.setAttribute('role', 'dialog')
  panel.setAttribute('aria-modal', 'true')
  panel.setAttribute('data-settings-dialog', '')
  document.body.append(panel)
}

const roots: Root[] = []

/** A real dialog from the shared Modal shell, the way every dialog and palette opens. */
function showModalDialog(): void {
  const host = document.createElement('div')
  document.body.append(host)
  const root = createRoot(host)
  roots.push(root)
  act(() => {
    root.render(
      createElement(Modal, {
        onClose: vi.fn(),
        children: createElement('button', null, 'Cancel')
      })
    )
  })
}

function showContextMenu(): void {
  const menu = document.createElement('div')
  menu.setAttribute('data-ctx-menu', '')
  document.body.append(menu)
}

beforeEach(() => setPlatform('darwin'))

afterEach(() => {
  for (const root of roots.splice(0)) act(() => root.unmount())
  document.body.replaceChildren()
})

describe('Close active tab in the main window (#893)', () => {
  it('closes the active tab when nothing is in front of it', () => {
    expect(resolveCloseShortcut(cmdW(), state())).toBe('close-tab')
  })

  it('closes the window once no tab is left (#192)', () => {
    expect(resolveCloseShortcut(cmdW(), state({ selectedPath: null }))).toBe('close-window')
  })

  it('closes Settings instead of the tab behind it', () => {
    showSettingsPanel()
    expect(resolveCloseShortcut(cmdW(), state({ settingsOpen: true }))).toBe('close-settings')
  })

  it('closes Settings instead of the window when no tab is open', () => {
    showSettingsPanel()
    expect(
      resolveCloseShortcut(cmdW(), state({ settingsOpen: true, selectedPath: null }))
    ).toBe('close-settings')
  })

  it('leaves Settings open while a dialog sits on top of it', () => {
    showSettingsPanel()
    showModalDialog()
    expect(resolveCloseShortcut(cmdW(), state({ settingsOpen: true }))).toBe('keep')
  })

  it.each([
    'searchOpen',
    'vaultTextSearchOpen',
    'commandPaletteOpen',
    'bufferPaletteOpen',
    'templatePaletteOpen',
    'embedDrawingPaletteOpen',
    'outlinePaletteOpen'
  ] as const)('never reaches through an open palette (%s)', (flag) => {
    expect(resolveCloseShortcut(cmdW(), state({ [flag]: true }))).toBe('keep')
    expect(resolveCloseShortcut(cmdW(), state({ [flag]: true, selectedPath: null }))).toBe('keep')
  })

  it('never reaches through an open dialog to the tab or the window behind it', () => {
    showModalDialog()
    expect(resolveCloseShortcut(cmdW(), state())).toBe('keep')
    expect(resolveCloseShortcut(cmdW(), state({ selectedPath: null }))).toBe('keep')
  })

  it('never reaches through an open context menu', () => {
    showContextMenu()
    expect(resolveCloseShortcut(cmdW(), state())).toBe('keep')
  })

  it('follows a rebind, Settings included', () => {
    showSettingsPanel()
    const keymapOverrides = { 'global.closeActiveTab': 'Shift+Mod+W' }
    expect(resolveCloseShortcut(cmdW(), state({ keymapOverrides, settingsOpen: true }))).toBeNull()
    expect(
      resolveCloseShortcut(
        keydown({ key: 'W', code: 'KeyW', metaKey: true, shiftKey: true }),
        state({ keymapOverrides, settingsOpen: true })
      )
    ).toBe('close-settings')
  })

  it('does nothing anywhere once the action is unbound', () => {
    const keymapOverrides = { 'global.closeActiveTab': UNBOUND_BINDING }
    expect(resolveCloseShortcut(cmdW(), state({ keymapOverrides }))).toBeNull()
    showSettingsPanel()
    expect(resolveCloseShortcut(cmdW(), state({ keymapOverrides, settingsOpen: true }))).toBeNull()
  })

  it('leaves Ctrl+W alone on macOS, where the shortcut is Cmd+W', () => {
    showSettingsPanel()
    expect(resolveCloseShortcut(ctrlW(), state({ settingsOpen: true }))).toBeNull()
  })

  it('matches by key position on a non-Latin layout', () => {
    showSettingsPanel()
    const cyrillicCmdW = keydown({ key: 'ц', code: 'KeyW', metaKey: true })
    expect(resolveCloseShortcut(cyrillicCmdW, state({ settingsOpen: true }))).toBe('close-settings')
  })

  describe('on Linux and Windows (Mod is Ctrl)', () => {
    beforeEach(() => setPlatform('linux'))

    it('keeps Ctrl+W as the Vim pane prefix while a tab is open', () => {
      expect(resolveCloseShortcut(ctrlW(), state())).toBe('vim')
    })

    it('closes the tab with Vim mode off', () => {
      expect(resolveCloseShortcut(ctrlW(), state({ vimMode: false }))).toBe('close-tab')
    })

    it('closes the window with Vim mode on once no tab is left (#192)', () => {
      expect(resolveCloseShortcut(ctrlW(), state({ selectedPath: null }))).toBe('close-window')
    })

    it('closes Settings even with Vim mode on, where the pane prefix has nothing to act on', () => {
      showSettingsPanel()
      expect(resolveCloseShortcut(ctrlW(), state({ settingsOpen: true }))).toBe('close-settings')
    })

    it('never closes the window behind a palette', () => {
      expect(
        resolveCloseShortcut(ctrlW(), state({ searchOpen: true, selectedPath: null }))
      ).toBe('keep')
    })
  })
})

describe('readSavedKeymapOverrides', () => {
  afterEach(() => localStorage.clear())

  it('reads what the main window saved, an unbind included', () => {
    localStorage.setItem(
      'zen:prefs:v2',
      JSON.stringify({ keymapOverrides: { 'global.closeActiveTab': UNBOUND_BINDING } })
    )
    expect(readSavedKeymapOverrides()).toEqual({ 'global.closeActiveTab': UNBOUND_BINDING })
  })

  it('falls back to the default bindings when the saved prefs are unreadable', () => {
    localStorage.setItem('zen:prefs:v2', '{not json')
    expect(readSavedKeymapOverrides()).toEqual({})
  })
})
