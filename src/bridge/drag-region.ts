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
 * upstream re-vendor (see GAP-ANALYSIS.md). As of v2.73:
 *
 *  - With "Window title bar" off (#754), the blank space of a top-edge pane's
 *    tab strip carries `.drag-region` instead of the title row, so it is
 *    covered without a selector of its own.
 *  - `.zn-app-dragging .drag-region` stands the strips down while a tab or a
 *    note is being dragged inside the app (the class sits on <html>).
 *  - The floating-note, Quick capture and external-file windows mark their
 *    header with an INLINE `WebkitAppRegion: 'drag'` style, which WebKitGTK
 *    drops on the floor, so nothing in the DOM says "drag" there. Their header
 *    is a `header.glass-header` directly inside the full-screen root column
 *    (`.h-screen`); every glass header in the main window sits deeper, inside
 *    a pane, so that structural selector stands in for the inline style.
 */

import { getCurrentWindow } from '@tauri-apps/api/window'

const DRAG_SELECTOR = '.drag-region, .h-screen > header.glass-header'
const NO_DRAG_SELECTOR = '.no-drag, button, a, input, textarea, [role="button"]'

export function installDragRegionShim(): void {
  window.addEventListener('mousedown', (event) => {
    if (event.button !== 0) return
    if (document.documentElement.classList.contains('zn-app-dragging')) return
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
