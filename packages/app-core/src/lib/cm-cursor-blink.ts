import { Compartment, type Extension } from '@codemirror/state'
import { ViewPlugin, drawSelection } from '@codemirror/view'

// CodeMirror's own default, so a blinking cursor looks as it always has.
const BLINK_RATE_MS = 1200

/**
 * drawSelection set from the Blinking cursor preference. The drawn caret and
 * the Vim block cursor both take their rate from this config, and 0 holds them
 * solid. Every editor builds its drawSelection here: a bare drawSelection()
 * blinks whatever the user chose, which is how the floating, Quick Capture and
 * external-file windows came to ignore it (#160, #892).
 */
export function cursorDrawSelection(blink: boolean): Extension {
  return drawSelection({ cursorBlinkRate: blink ? BLINK_RATE_MS : 0 })
}

/**
 * cursorDrawSelection for a window that seeds its editor from the stored prefs
 * blob instead of the store (floating note, Quick Capture, external file).
 * Settings saves the blob in the main window, and another window learns of
 * that only through the `storage` event, so the change lands here while the
 * window is open. Quick Capture is hidden rather than closed: reading the blob
 * only at mount would hold its first value all session.
 */
export function storedCursorDrawSelection(
  prefsKey: string,
  readBlink: () => boolean
): Extension {
  const compartment = new Compartment()
  const initial = readBlink()
  return [
    compartment.of(cursorDrawSelection(initial)),
    ViewPlugin.define((view) => {
      let blink = initial
      const onStorage = (event: StorageEvent): void => {
        // A null key is another window clearing localStorage.
        if (event.key !== null && event.key !== prefsKey) return
        const next = readBlink()
        // Every preference saves the whole blob, so most of these events are
        // about another one.
        if (next === blink) return
        blink = next
        view.dispatch({ effects: compartment.reconfigure(cursorDrawSelection(blink)) })
      }
      window.addEventListener('storage', onStorage)
      return { destroy: () => window.removeEventListener('storage', onStorage) }
    })
  ]
}
