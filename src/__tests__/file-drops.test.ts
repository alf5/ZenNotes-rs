// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'

const tauriInvoke = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauriInvoke }))
vi.mock('@tauri-apps/api/webview', () => ({ getCurrentWebview: vi.fn() }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }))

// jsdom has neither; WebKitGTK's were probed for exactly this use (2.54).
class FakeTransfer {
  files: File[] = []
  items = {
    add: (file: File) => {
      this.files.push(file)
    }
  }
  types = ['Files']
}
class FakeDragEvent extends Event {
  dataTransfer: unknown
  clientX: number
  clientY: number
  constructor(type: string, init: EventInit & { dataTransfer?: unknown; clientX?: number; clientY?: number }) {
    super(type, init)
    this.dataTransfer = init.dataTransfer
    this.clientX = init.clientX ?? 0
    this.clientY = init.clientY ?? 0
  }
}
vi.stubGlobal('DataTransfer', FakeTransfer)
vi.stubGlobal('DragEvent', FakeDragEvent)
document.elementFromPoint = () => document.body

const { deliverPageDrop, dispatchFileDrop, pathForDroppedFile, pathsFromUriList } = await import(
  '../bridge/file-drops'
)

function nextDrop(): Promise<FakeDragEvent> {
  return new Promise((resolve) => {
    document.addEventListener('drop', (event) => resolve(event as FakeDragEvent), { once: true })
  })
}

const file = (name: string): File => new File(['bytes'], name)

describe('a synthetic drop', () => {
  it('carries stand-in files that getPathForFile can answer for', async () => {
    const seen = nextDrop()
    dispatchFileDrop(['/v/My Photo.png', 'C:\\docs\\b.pdf'], 12, 34)
    const event = await seen
    const files = (event.dataTransfer as FakeTransfer).files
    expect(files.map((f) => f.name)).toEqual(['My Photo.png', 'b.pdf'])
    expect(files.map(pathForDroppedFile)).toEqual(['/v/My Photo.png', 'C:\\docs\\b.pdf'])
    expect([event.clientX, event.clientY]).toEqual([12, 34])
  })

  it('knows nothing about a File it did not make', () => {
    expect(pathForDroppedFile(file('real.png'))).toBeNull()
  })
})

describe('pathsFromUriList', () => {
  it('reads file URLs on every platform and skips the rest', () => {
    expect(
      pathsFromUriList(
        '# comment\r\nfile:///home/a/b%20c.png\r\nhttps://example.com/x\r\nfile:///C:/d/e.png\n\nnot a url'
      )
    ).toEqual(['/home/a/b c.png', 'C:/d/e.png'])
  })
})

describe('a drop the page received (macOS, Windows)', () => {
  beforeEach(() => {
    tauriInvoke.mockReset()
    vi.spyOn(window, 'alert').mockImplementation(() => {})
    vi.mocked(window.alert).mockClear()
    File.prototype.arrayBuffer ??= async () => new ArrayBuffer(0)
  })

  it('uses real paths when the webview gives them', async () => {
    tauriInvoke.mockResolvedValueOnce([]) // app_dropped_folders
    const seen = nextDrop()
    await deliverPageDrop({ files: [file('a.md')], uriPaths: ['/v/a.md'], hasFolder: false }, 1, 2)
    const dropped = ((await seen).dataTransfer as FakeTransfer).files[0] as File
    expect(pathForDroppedFile(dropped)).toBe('/v/a.md')
  })

  it('stages an attachment with no path, under its encoded name', async () => {
    tauriInvoke.mockResolvedValueOnce('/tmp/zennotes-rs-drops/x/ä b.png')
    const seen = nextDrop()
    await deliverPageDrop({ files: [file('ä b.png')], uriPaths: [], hasFolder: false }, 1, 2)
    const dropped = ((await seen).dataTransfer as FakeTransfer).files[0] as File
    expect(pathForDroppedFile(dropped)).toBe('/tmp/zennotes-rs-drops/x/ä b.png')
    expect(tauriInvoke.mock.calls[0]?.[0]).toBe('app_stage_dropped_file')
    expect(tauriInvoke.mock.calls[0]?.[1]).toBeInstanceOf(Uint8Array)
    expect(tauriInvoke.mock.calls[0]?.[2]).toEqual({ headers: { 'x-file-name': '%C3%A4%20b.png' } })
  })

  it.each([
    ['a Markdown file', { files: [file('a.md')], uriPaths: [], hasFolder: false }],
    ['a folder', { files: [file('dir')], uriPaths: [], hasFolder: true }]
  ])('refuses %s with no path rather than opening a temp copy', async (_name, drop) => {
    await deliverPageDrop(drop, 1, 2)
    expect(window.alert).toHaveBeenCalledOnce()
    expect(tauriInvoke).not.toHaveBeenCalled()
  })
})
