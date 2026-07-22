/**
 * Window-drag shim for undecorated windows (Linux).
 *
 * app-core marks its custom titlebar with Electron's `-webkit-app-region:
 * drag` CSS (`.drag-region` in packages/app-core/src/styles/index.css), which
 * Electron honors natively but WebKitGTK does not parse at all. This shim
 * reproduces the behavior: primary-button mousedown on a `.drag-region`
 * starts a compositor move, double-click toggles maximize, and anything
 * matching the stylesheet's no-drag selectors is exempt.
 *
 * The selectors mirror the vendored stylesheet — re-check them after each
 * upstream re-vendor (see GAP-ANALYSIS.md).
 */

import { getCurrentWindow } from '@tauri-apps/api/window'

const DRAG_SELECTOR = '.drag-region'
const NO_DRAG_SELECTOR = '.no-drag, button, a, input, textarea, [role="button"]'

export function installDragRegionShim(): void {
  window.addEventListener('mousedown', (event) => {
    if (event.button !== 0) return
    const target = event.target as Element | null
    if (!target?.closest?.(DRAG_SELECTOR)) return
    if (target.closest(NO_DRAG_SELECTOR)) return
    const win = getCurrentWindow()
    if (event.detail >= 2) {
      void win.toggleMaximize().catch(() => {})
    } else {
      void win.startDragging().catch(() => {})
    }
  })
}
