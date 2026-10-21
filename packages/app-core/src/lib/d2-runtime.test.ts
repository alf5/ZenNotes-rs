import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { describe, expect, it } from 'vitest'
import { pageWorkerScript } from './d2-runtime'

describe('pageWorkerScript', () => {
  it('drops the module exports and hands the handler the port', () => {
    const source = [
      'export let BrotliDecode = (() => 1)();',
      'export function setupMessageHandler(isNode, port, initWasm) {}',
      'async function initWasmBrowser() { return self.d2 }',
      'setupMessageHandler(false, self, initWasmBrowser);'
    ].join('\n')
    const script = pageWorkerScript(source)
    expect(script).not.toBeNull()
    expect(script).not.toMatch(/^\s*export\s/m)
    expect(script).toContain('setupMessageHandler(false, port, initWasmBrowser);')
    expect(() => new Function('port', script!)).not.toThrow()
  })

  it('refuses a script without the entry it expects', () => {
    expect(pageWorkerScript('setupMessageHandler(true, parentPort, initWasmNode);')).toBeNull()
  })

  it('turns the worker script of the installed @terrastruct/d2 into a valid function body', async () => {
    // The browser build assembles its worker from a Blob of strings as a D2
    // is constructed. Record that Blob, then refuse the Worker, so nothing
    // starts and the compiler is never decoded.
    const require = createRequire(import.meta.url)
    const browserBuild = join(dirname(require.resolve('@terrastruct/d2')), '..', 'browser', 'index.js')
    const { D2 } = (await import(/* @vite-ignore */ pathToFileURL(browserBuild).href)) as {
      D2: new () => { ready: Promise<void> }
    }
    let source: string | null = null
    const saved = { Blob: globalThis.Blob, Worker: globalThis.Worker }
    globalThis.Blob = class extends saved.Blob {
      constructor(parts?: BlobPart[], options?: BlobPropertyBag) {
        super(parts, options)
        if (parts?.every((part) => typeof part === 'string')) source = parts.join('')
      }
    }
    globalThis.Worker = function NoWorker() {
      throw new Error('not started in tests')
    } as unknown as typeof Worker
    let ready: Promise<void>
    try {
      ready = new D2().ready
    } finally {
      globalThis.Blob = saved.Blob
      globalThis.Worker = saved.Worker
    }
    await expect(ready).rejects.toThrow()
    expect(source).not.toBeNull()
    const script = pageWorkerScript(source!)
    expect(script).not.toBeNull()
    expect(script).not.toMatch(/^\s*export\s/m)
    // Parsing only: the Function is built, never called.
    expect(() => new Function('port', script!)).not.toThrow()
  })
})
