import { renderZenNotesApp } from '@zennotes/app-core/main'
import { createTauriBridge } from './bridge/tauri-bridge'
import { renderExportNoteWindow } from './export-window'

// Install the Tauri-backed bridge. installZenBridge also assigns it to
// window.zen, so components that still read window.zen directly (e.g. the
// export window) keep working — the single behavioral change vs. the
// Electron renderer entry is the bridge implementation.
createTauriBridge()

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
    renderExportNoteWindow(root, exportNotePath)
  } else {
    renderZenNotesApp(root)
  }
} catch (error) {
  console.error('[zennotes-rs-renderer] boot failed', error)
  renderBootError(String(error instanceof Error ? error.stack ?? error.message : error))
}
