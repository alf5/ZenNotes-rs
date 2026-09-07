import { initPortableConfig } from './bridge/portable-config'

// Load config.toml before app-core evaluates: its store reads getConfigSync()
// synchronously while building the initial state, so the snapshot must exist
// by then. Electron pre-loads it in main before the window opens; here one
// awaited invoke does the same job.
//
// EVERYTHING else is imported dynamically, below this await, and that is
// load-bearing rather than stylistic. A static import is evaluated before this
// module's body runs, so anything Rollup happens to place in the same chunk as
// app-core's store would evaluate `loadPrefs()` before the config snapshot
// exists — and `loadPrefs` memoizes its result, so the file would be ignored
// for the whole session (config.toml never applied, the first-run wizard shown
// on every launch). Importing `./bridge/tauri-bridge` statically did exactly
// that: it pulls shared-domain helpers, which Rollup packed into the
// `app-store` chunk alongside the store.
//
// Keeping this module's static graph down to `portable-config` (plus smol-toml)
// is what makes the ordering a property of the code rather than of chunking
// luck; `bootstrapChunk` in vite.config.ts pins that split so a future
// re-vendor cannot quietly merge the two again. `main-bootstrap-order.test.ts`
// fails the build if a static import creeps back in.
try {
  await initPortableConfig()
} catch (error) {
  console.error('[zennotes-rs-renderer] config bootstrap failed', error)
}

// Install the Tauri-backed bridge. installZenBridge also assigns it to
// window.zen, so components that still read window.zen directly (e.g. the
// export window) keep working — the single behavioral change vs. the
// Electron renderer entry is the bridge implementation.
const { createTauriBridge } = await import('./bridge/tauri-bridge')
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
const { installDragRegionShim } = await import('./bridge/drag-region')
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
