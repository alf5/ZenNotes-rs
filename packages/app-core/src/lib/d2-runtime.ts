/**
 * The one module that imports D2 (d2lang.com), reached only through the
 * `import()` in `d2-render`.
 *
 * It exists so the renderer's async chunk carries a name of ours,
 * `d2-runtime-*.js`: the browser build of `@terrastruct/d2` is one file of
 * about 8 MB with the compiler's WebAssembly inside it, and the perf harness
 * and the packaged-web browser test assert that this chunk is not fetched
 * before a note needs a D2 diagram.
 *
 * It also decides where the compiler runs. `@terrastruct/d2` starts it in a
 * worker, and in WebKit (the iPhone app, Safari on the self-hosted web app)
 * the Go runtime overflows a worker's smaller stack while it starts: the
 * WebAssembly loads, the worker still reports ready, and every diagram then
 * fails with "undefined is not an object (evaluating 'd2.compile')". On the
 * page's own thread it starts fine. So a D2 whose worker cannot answer is
 * replaced by one whose worker script runs on the page.
 */
import { D2 } from '@terrastruct/d2'

export { D2 }

/** The worker script's last line: it hands the message handler the worker's
 *  own global as the port it talks through. */
const WORKER_ENTRY = 'setupMessageHandler(false, self, initWasmBrowser);'

/**
 * The worker script `@terrastruct/d2` builds, rewritten to run as a function
 * on the page: its two `export` keywords dropped (it is a module script, and
 * a function body cannot export) and its handler given the `port` argument
 * instead of the worker's global. No blob URL is fetched or imported, which
 * the self-hosted server's Content-Security-Policy would refuse; running it
 * needs only the 'unsafe-eval' the script already needs for its layout
 * library. Null when the script no longer has the entry this expects, so an
 * update of the package fails loudly instead of running something else.
 */
export function pageWorkerScript(source: string): string | null {
  if (!source.includes(WORKER_ENTRY)) return null
  return source
    .replace(/^(\s*)export\s+(?=(let|const|function|class)\b)/gm, '$1')
    .replace(WORKER_ENTRY, 'setupMessageHandler(false, port, initWasmBrowser);')
}

interface MessageTarget {
  postMessage(data: unknown): void
  onmessage: ((event: { data: unknown }) => void) | null
}

/** Stands in for the Worker `@terrastruct/d2` creates: runs its script on the
 *  page and passes messages both ways a tick later, as a worker would. */
class PageWorker {
  onmessage: ((event: { data: unknown }) => void) | null = null
  onerror: ((event: unknown) => void) | null = null
  private readonly port: MessageTarget

  constructor(script: string) {
    this.port = {
      postMessage: (data) => {
        setTimeout(() => this.onmessage?.({ data }))
      },
      onmessage: null
    }
    new Function('port', script)(this.port)
  }

  postMessage(data: unknown): void {
    setTimeout(() => this.port.onmessage?.({ data }))
  }

  terminate(): void {}
}

/** Globals the Go runtime's `wasm_exec` stubs out when the page lacks them.
 *  Go reads them once while it starts and keeps its own references, so they
 *  can leave the page afterwards; a `process` without `env` would otherwise
 *  mislead every library that checks for Node. `fs` stays: the runtime writes
 *  its output through the bare name. */
const GO_STUBS = ['process', 'path'] as const

/** A D2 whose worker script runs on the page. `new D2()` builds its worker
 *  from a Blob of the script and starts it in the same synchronous step, so
 *  Blob and Worker are swapped only for that step: the Blob keeps the
 *  script's text, and the Worker runs it here. */
async function onThePage(): Promise<D2> {
  const OriginalBlob = globalThis.Blob
  const OriginalWorker = globalThis.Worker
  const absent = GO_STUBS.filter((name) => !(name in globalThis))
  let script: string | null = null
  globalThis.Blob = class extends OriginalBlob {
    constructor(parts?: BlobPart[], options?: BlobPropertyBag) {
      super(parts, options)
      if (parts?.every((part) => typeof part === 'string')) {
        script = pageWorkerScript(parts.join('')) ?? script
      }
    }
  }
  globalThis.Worker = function PageWorkerFactory() {
    if (!script) throw new Error('The D2 worker script is not the one this version expects')
    return new PageWorker(script)
  } as unknown as typeof Worker
  let d2: D2
  try {
    d2 = new D2()
  } finally {
    globalThis.Blob = OriginalBlob
    globalThis.Worker = OriginalWorker
  }
  try {
    await (d2 as unknown as Probe).ready
  } finally {
    for (const name of absent) delete (globalThis as Record<string, unknown>)[name]
  }
  return d2
}

/** What `@terrastruct/d2` has at runtime but leaves out of its typings. */
interface Probe {
  ready: Promise<void>
  version(): Promise<unknown>
  worker?: { terminate?(): void }
}

/** A D2 that answers: the package's own, in its worker, when that worker
 *  starts; otherwise one on the page. The probe is `version()`, the cheapest
 *  call that reaches the compiler. */
export async function createD2(): Promise<D2> {
  const inWorker = new D2()
  try {
    await (inWorker as unknown as Probe).version()
    return inWorker
  } catch {
    ;(inWorker as unknown as Probe).worker?.terminate?.()
    return onThePage()
  }
}
