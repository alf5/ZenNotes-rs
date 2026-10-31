import { historyKeymap, redo } from '@codemirror/commands'
import type { KeyBinding } from '@codemirror/view'

/**
 * CodeMirror's undo/redo keys, with Ctrl+Shift+Z redoing on Windows as well.
 * CodeMirror binds it on Linux and Cmd+Shift+Z on macOS, but on Windows only
 * Ctrl+Y, while the editor's context menu shows Mod+Shift+Z everywhere and
 * that is what Windows apps use too (#967). Ctrl+Y keeps working.
 */
export const appHistoryKeymap: readonly KeyBinding[] = [
  ...historyKeymap,
  { win: 'Ctrl-Shift-z', run: redo, preventDefault: true }
]
