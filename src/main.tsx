import { createTauriBridge } from './bridge/tauri-bridge'
import { installDragRegionShim } from './bridge/drag-region'
import { initPortableConfig } from './bridge/portable-config'

// Load config.toml before app-core evaluates: its store reads getConfigSync()
// synchronously while building the initial state, so the snapshot must exist
// by then. Electron pre-loads it in main before the window opens; here one
// awaited invoke does the same job — which is exactly why app-core is
// imported DYNAMICALLY below (a static import would evaluate the store
// before this top-level await resolves). A failed load falls back to
// pure-localStorage prefs rather than blocking boot.
try {
  await initPortableConfig()
} catch (error) {
  console.error('[zennotes-rs-renderer] config bootstrap failed', error)
}

// Install the Tauri-backed bridge. installZenBridge also assigns it to
// window.zen, so components that still read window.zen directly (e.g. the
// export window) keep working — the single behavioral change vs. the
// Electron renderer entry is the bridge implementation.
createTauriBridge()

// Point Excalidraw's font loader at the bundled same-origin copy instead of
// its default esm.sh CDN, which the webview CSP blocks (upstream #324). Must
// be set before the lazy Excalidraw bundle loads; it appends
// `fonts/<Family>/<file>` to this base. Resolved against the current document
// so it works both under the Vite dev server and Tauri's bundle protocol.
const excalidrawGlobal = window as unknown as { EXCALIDRAW_ASSET_PATH?: string }
excalidrawGlobal.EXCALIDRAW_ASSET_PATH = new URL('excalidraw-assets/', window.location.href).toString()

// The native frame is disabled on Linux (tauri.linux.conf.json + the window
// builders); the in-app titlebar handles move/maximize through this shim.
installDragRegionShim()

const root = document.getElementById('root')

function renderBootError(message: string): void {
  if (!root) return
  root.replaceChildren()
  const pre = document.createElement('pre')
  pre.style.padding = '24px'
  pre.style.color = '#b42318'
  pre.style.background = '#fff7f7'
  pre.style.font = '14px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace'
  pre.style.whiteSpace = 'pre-wrap'
  pre.textContent = message
  root.appendChild(pre)
}

window.addEventListener('error', (event) => {
  console.error('[zennotes-rs-renderer] uncaught error', event.error ?? event.message)
})

window.addEventListener('unhandledrejection', (event) => {
  console.error('[zennotes-rs-renderer] unhandled rejection', event.reason)
})

try {
  if (!root) {
    throw new Error('Renderer root element #root was not found')
  }
  const params = new URLSearchParams(window.location.search)
  const exportNotePath = params.get('exportNote')
  if (exportNotePath) {
    const { renderExportNoteWindow } = await import('./export-window')
    renderExportNoteWindow(root, exportNotePath)
  } else {
    const { renderZenNotesApp } = await import('@zennotes/app-core/main')
    renderZenNotesApp(root)
  }
} catch (error) {
  console.error('[zennotes-rs-renderer] boot failed', error)
  renderBootError(String(error instanceof Error ? error.stack ?? error.message : error))
}
