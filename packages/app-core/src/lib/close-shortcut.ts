import { useEffect, useRef, useState } from 'react'
import {
  getKeymapDisplay,
  matchesSequenceToken,
  matchesShortcut,
  normalizeKeymapOverrides,
  type KeymapOverrides
} from './keymaps'
import { isAppOverlayOpen } from './overlay-open'

const PREFS_KEY = 'zen:prefs:v2'

/**
 * What the Close active tab shortcut (Mod+W unless rebound) does in the main
 * window at the moment it is pressed.
 *
 * It closes the frontmost window, the way Cmd+W does everywhere on the Mac.
 * Settings counts as one, because every other Mac app gives Settings its own
 * window that Cmd+W closes (#893). A dialog, palette or menu is not a window:
 * while one is open the shortcut is spent on nothing, so it can never close
 * the tab hidden behind it, or the whole window once no tab is left. Escape
 * stays the way out of those.
 */
export type CloseShortcutAction =
  | 'close-settings'
  | 'keep'
  | 'vim'
  | 'close-tab'
  | 'close-window'

export interface CloseShortcutState {
  keymapOverrides: KeymapOverrides
  vimMode: boolean
  selectedPath: string | null
  settingsOpen: boolean
  searchOpen: boolean
  vaultTextSearchOpen: boolean
  commandPaletteOpen: boolean
  bufferPaletteOpen: boolean
  templatePaletteOpen: boolean
  embedDrawingPaletteOpen: boolean
  outlinePaletteOpen: boolean
}

/**
 * True while a dialog or menu other than Settings is open. The shared Modal
 * shell marks every panel it draws aria-modal, which covers the dialogs and
 * palettes that carry no data-* hook of their own (folder pickers, Publish,
 * the workflow step picker); context menus carry data-ctx-menu. Settings draws
 * its own panel and tells itself apart with data-settings-dialog.
 */
export function isDialogOrMenuOpen(): boolean {
  if (typeof document === 'undefined') return false
  return (
    isAppOverlayOpen() ||
    document.querySelector('[aria-modal="true"]:not([data-settings-dialog])') !== null
  )
}

function isPaletteOpen(state: CloseShortcutState): boolean {
  // The store flags, not only the DOM: a palette is lazily loaded, and a key
  // pressed before its chunk mounts must not land on the tab behind it.
  return (
    state.searchOpen ||
    state.vaultTextSearchOpen ||
    state.commandPaletteOpen ||
    state.bufferPaletteOpen ||
    state.templatePaletteOpen ||
    state.embedDrawingPaletteOpen ||
    state.outlinePaletteOpen
  )
}

export function resolveCloseShortcut(
  event: KeyboardEvent,
  state: CloseShortcutState
): CloseShortcutAction | null {
  const overrides = state.keymapOverrides
  if (!matchesShortcut(event, overrides, 'global.closeActiveTab')) return null
  if (isPaletteOpen(state) || isDialogOrMenuOpen()) return 'keep'
  // Ahead of the Vim guard below: with Settings in front, the pane prefix has
  // no pane to act on, so Ctrl+W closes Settings on Linux and Windows too.
  if (state.settingsOpen) return 'close-settings'
  // On Linux/Windows `Mod+W` (close tab) resolves to Ctrl+W, which is also
  // the vim pane-focus prefix (`<C-w>hjkl`) and insert-mode word delete.
  // When vim mode is on AND a tab is open, reserve Ctrl+W for vim (close
  // tabs via :q / :bd / the palette). With no tab open the prefix has
  // nothing to act on, so fall through and close the window. On macOS
  // close-tab is Cmd+W, so the vim guard never matches there.
  const hasActiveTab = !!state.selectedPath
  if (state.vimMode && hasActiveTab && matchesSequenceToken(event, overrides, 'vim.panePrefix')) {
    return 'vim'
  }
  // No tab left to close: close the window, matching native Cmd+W (macOS) /
  // Ctrl+W behavior even with vim mode on (#192).
  return hasActiveTab ? 'close-tab' : 'close-window'
}

/**
 * The keymap overrides the main window last saved. A floating note, Quick
 * Capture and a standalone file window never hydrate the store, but they share
 * its localStorage, so a rebind or an unbind made in Settings reaches them
 * through the same prefs blob.
 */
export function readSavedKeymapOverrides(): KeymapOverrides {
  try {
    const raw = localStorage.getItem(PREFS_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as { keymapOverrides?: unknown } | null
    return normalizeKeymapOverrides(parsed?.keymapOverrides)
  } catch {
    return {}
  }
}

/**
 * Close a window that has no tabs (a floating note, Quick Capture, a
 * standalone file) on the Close active tab shortcut, through the same path as
 * its close button. Its one note is its only tab, so closing that tab closes
 * the window, as the main window does once no tab is left (#192). The binding
 * is read the way the main window reads it, by key position on non-Latin
 * layouts and Cmd on macOS but Ctrl elsewhere, and a rebind or an unbind
 * applies here too (#893). A dialog or menu open in the window, or whatever
 * `isBlocked` reports, keeps the key, as it does in the main window.
 *
 * `capture` is for windows whose editor swallows the chord first. Quick capture
 * sets it because codemirror-vim binds Ctrl+W as its window-command prefix and
 * stops propagation, so the bubble phase never sees it. The main window leaves
 * it off: it already resolves that chord in its own capture-phase handler, and
 * listening here in capture too would run both.
 */
export function useCloseWindowShortcut(
  isBlocked?: () => boolean,
  capture = false
): void {
  const isBlockedRef = useRef(isBlocked)
  useEffect(() => {
    isBlockedRef.current = isBlocked
  }, [isBlocked])

  useEffect(() => {
    let overrides = readSavedKeymapOverrides()
    // Quick Capture is hidden, never closed, so it lives for the whole
    // session: a binding changed meanwhile must reach it without a restart.
    const onStorage = (event: StorageEvent): void => {
      if (event.key === null || event.key === PREFS_KEY) {
        overrides = readSavedKeymapOverrides()
      }
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (!matchesShortcut(event, overrides, 'global.closeActiveTab')) return
      event.preventDefault()
      if (isDialogOrMenuOpen() || isBlockedRef.current?.()) return
      window.zen.windowClose()
    }
    window.addEventListener('storage', onStorage)
    window.addEventListener('keydown', onKeyDown, capture)
    return () => {
      window.removeEventListener('storage', onStorage)
      window.removeEventListener('keydown', onKeyDown, capture)
    }
  }, [capture])
}

/**
 * The Close active tab binding as a footer shows it, or the empty string once
 * it is unbound, so a window that advertises the key can leave the hint out
 * rather than name a key that does nothing. Read and kept current the way
 * useCloseWindowShortcut reads its binding.
 */
export function useCloseShortcutDisplay(): string {
  const [display, setDisplay] = useState(() =>
    getKeymapDisplay(readSavedKeymapOverrides(), 'global.closeActiveTab')
  )
  useEffect(() => {
    const onStorage = (event: StorageEvent): void => {
      if (event.key === null || event.key === PREFS_KEY) {
        setDisplay(getKeymapDisplay(readSavedKeymapOverrides(), 'global.closeActiveTab'))
      }
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [])
  return display
}
