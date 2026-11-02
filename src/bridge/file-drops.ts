/**
 * Files dragged in from the OS. app-core handles a DOM `drop` carrying `File`
 * objects and asks `getPathForFile` for each one's path, which is Electron's
 * `webUtils.getPathForFile`; no webview here can answer that for a real `File`.
 *
 * - Linux: Tauri's native handler takes the drop and reports real paths, and
 *   the page never sees it. The internal drags keep working beside it.
 * - macOS and Windows: that handler also swallows every in-app drag (tabs,
 *   sidebar, boards), so it is off (tauri.conf.json, windows.rs) and the page
 *   gets the drop, with no paths. A `text/uri-list` is used when the webview
 *   gives one; otherwise attachments are staged to a temp file for the importer.
 *   A Markdown file or folder must be opened where it lies, so without a real
 *   path it is refused with a message rather than opened from a temp copy.
 *
 * Either way app-core receives one synthetic `drop` of stand-in `File`s, and
 * `pathForDroppedFile` knows their paths.
 */
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { invoke } from './ipc'

const stagedPaths = new WeakMap<File, string>()

/** `getPathForFile`: the path behind a stand-in, or null for any other File. */
export function pathForDroppedFile(file: File): string | null {
  return stagedPaths.get(file) ?? null
}

function baseName(path: string): string {
  return path.split(/[/\\]/).filter(Boolean).pop() ?? path
}

/** Deliver paths to whatever is under the point, as app-core expects a drop. */
export function dispatchFileDrop(paths: string[], clientX: number, clientY: number): void {
  const transfer = new DataTransfer()
  for (const path of paths) transfer.items.add(new File([], baseName(path)))
  // `files` hands out its own objects, so those are the ones to remember.
  Array.from(transfer.files).forEach((file, index) => {
    const path = paths[index]
    if (path) stagedPaths.set(file, path)
  })
  const target = document.elementFromPoint(clientX, clientY) ?? document.body
  target.dispatchEvent(
    new DragEvent('drop', { bubbles: true, cancelable: true, clientX, clientY, dataTransfer: transfer })
  )
}

/** The main workspace window is the one that opens a dropped folder. */
function isWorkspaceWindow(): boolean {
  return window.location.search === ''
}

async function deliverPaths(paths: string[], clientX: number, clientY: number): Promise<void> {
  const folders = new Set(await invoke<string[]>('app_dropped_folders', { paths }))
  if (isWorkspaceWindow()) {
    for (const folder of folders) {
      await window.zen.openFolderTemporary(folder).catch((err: Error) => window.alert(err.message))
    }
  }
  const files = paths.filter((path) => !folders.has(path))
  if (files.length > 0) dispatchFileDrop(files, clientX, clientY)
}

/**
 * Native positions are window coordinates; a client point is scaled by the app
 * zoom too. The webview fills the window, so the width ratio gives that zoom.
 */
async function clientPoint(position: { x: number; y: number }): Promise<{ x: number; y: number }> {
  const win = getCurrentWindow()
  const [size, scale] = await Promise.all([win.innerSize(), win.scaleFactor()])
  const zoom = size.width / scale / window.innerWidth
  const factor = Number.isFinite(zoom) && zoom > 0 ? zoom : 1
  return { x: position.x / factor, y: position.y / factor }
}

function installNativeDrops(): void {
  void getCurrentWebview()
    .onDragDropEvent(async ({ payload }) => {
      if (payload.type !== 'drop' || payload.paths.length === 0) return
      const point = await clientPoint(payload.position)
      await deliverPaths(payload.paths, point.x, point.y)
    })
    .catch((err) => console.error('[zennotes-rs] file drops are off', err))
}

/** `file://` lines of a uri-list, as paths. Windows' `/C:/x` loses its slash. */
export function pathsFromUriList(raw: string): string[] {
  const paths: string[] = []
  for (const line of raw.split(/\r?\n/)) {
    const value = line.trim()
    if (!value || value.startsWith('#')) continue
    try {
      const url = new URL(value)
      if (url.protocol !== 'file:') continue
      const path = decodeURIComponent(url.pathname)
      paths.push(/^\/[A-Za-z]:/.test(path) ? path.slice(1) : path)
    } catch {
      continue
    }
  }
  return paths
}

const NEEDS_A_REAL_PATH = /\.(md|markdown)$/i

async function stageFile(file: File): Promise<string> {
  return invoke<string>('app_stage_dropped_file', new Uint8Array(await file.arrayBuffer()), {
    headers: { 'x-file-name': encodeURIComponent(file.name) }
  })
}

/** What the page's own `drop` event holds, read while the event is still live. */
export interface PageDrop {
  files: File[]
  uriPaths: string[]
  hasFolder: boolean
}

export function readPageDrop(transfer: DataTransfer): PageDrop {
  return {
    files: Array.from(transfer.files),
    uriPaths: pathsFromUriList(transfer.getData('text/uri-list')),
    hasFolder: Array.from(transfer.items ?? []).some(
      (item) => item.kind === 'file' && item.webkitGetAsEntry?.()?.isDirectory === true
    )
  }
}

export async function deliverPageDrop(drop: PageDrop, clientX: number, clientY: number): Promise<void> {
  if (drop.uriPaths.length > 0 && drop.uriPaths.length === drop.files.length) {
    await deliverPaths(drop.uriPaths, clientX, clientY)
    return
  }
  if (drop.hasFolder || drop.files.some((file) => NEEDS_A_REAL_PATH.test(file.name))) {
    window.alert(
      'This system does not tell ZenNotes-rs where a dropped Markdown file or folder is. Use Open File… for a note, or open the folder as a vault.'
    )
    return
  }
  const staged = await Promise.all(drop.files.map(stageFile))
  if (staged.length > 0) dispatchFileDrop(staged, clientX, clientY)
}

function installPageDrops(): void {
  window.addEventListener(
    'drop',
    (event) => {
      if (!event.isTrusted || !event.dataTransfer) return
      if (!Array.from(event.dataTransfer.types).includes('Files')) return
      // Ahead of app-core, which would find no path: it gets the synthetic one.
      event.preventDefault()
      event.stopImmediatePropagation()
      const { clientX, clientY } = event
      deliverPageDrop(readPageDrop(event.dataTransfer), clientX, clientY).catch((err: Error) =>
        window.alert(err.message)
      )
    },
    true
  )
}

export function installFileDrops(platform: string): void {
  if (platform === 'linux') installNativeDrops()
  else installPageDrops()
}
