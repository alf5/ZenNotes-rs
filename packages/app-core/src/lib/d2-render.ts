/**
 * D2 diagrams (d2lang.com), shared by every surface that draws one: the
 * Preview pipeline and the editor's live preview.
 *
 * A ```d2 fence is compiled and laid out by D2 itself, through the WebAssembly
 * build in `@terrastruct/d2`, which runs in a worker of its own. The module is
 * about 8 MB, so it loads only when a note holds a d2 fence (see `d2-runtime`).
 *
 * Three things here are not optional:
 *
 *   - One request at a time. The wrapper keeps a single pending callback per
 *     worker, so a second call made before the first settles overwrites it and
 *     the first never settles. Every call goes through `enqueue`.
 *   - Sanitizing. D2 passes the HTML of a Markdown label straight into the SVG
 *     (an `<img onerror>` in a label arrives intact), draws `icon:` as an
 *     `<image>` that fetches its URL, and wraps `link:` in an `<a>`. A note must
 *     never run script, reach the network or navigate by being looked at, which
 *     is what Mermaid's strict mode guaranteed and Pele keeps, so the SVG goes
 *     through DOMPurify with images and links removed before any DOM sees it.
 *   - Colour comes from the theme. D2 paints through classes (`fill-N1`,
 *     `stroke-B1` and so on) that `index.css` maps onto the `--z-*` tokens, so a
 *     diagram matches the app and a theme switch repaints it without a redraw.
 *     A diagram that picks a D2 theme of its own (`theme-id` in `d2-config`)
 *     keeps it: the result says so, and the caller leaves the mapping off.
 */
import DOMPurify, { type DOMPurify as DOMPurifyInstance } from 'dompurify'

type D2Module = typeof import('./d2-runtime')
type D2Instance = InstanceType<D2Module['D2']>

let d2Promise: Promise<D2Instance> | null = null

function loadD2(): Promise<D2Instance> {
  if (!d2Promise) {
    d2Promise = import('./d2-runtime').then((module) => module.createD2())
    // A failed load (an offline web client that never fetched the chunk) must
    // not poison every later diagram.
    d2Promise.catch(() => {
      d2Promise = null
    })
  }
  return d2Promise
}

let queue: Promise<unknown> = Promise.resolve()

function enqueue<T>(job: () => Promise<T>): Promise<T> {
  const run = queue.then(job, job)
  queue = run.catch(() => undefined)
  return run
}

export type D2RenderResult =
  | { ok: true; svg: string; ownTheme: boolean }
  | { ok: false; error: string }

const svgCache = new Map<string, D2RenderResult>()
const inFlight = new Map<string, Promise<D2RenderResult>>()
/** Diagrams are big strings, fonts included; the number is small on purpose. */
const SVG_CACHE_LIMIT = 40

/** The editor trims a fence's body and Preview keeps its trailing newline;
 *  whitespace around a diagram never changes it, so both share one entry. */
function normalizeSource(source: string): string {
  return source.trim()
}

function remember(key: string, result: D2RenderResult): D2RenderResult {
  if (svgCache.size >= SVG_CACHE_LIMIT) {
    const oldest = svgCache.keys().next().value
    if (oldest !== undefined) svgCache.delete(oldest)
  }
  svgCache.set(key, result)
  return result
}

/** A `d2-config` that names a theme of D2's own wins over the app palette. */
export function d2SourceSetsTheme(source: string): boolean {
  return /(^|[\s{;])(dark-)?theme-(id|overrides)\s*:/m.test(source)
}

function sourceSets(source: string, key: string): boolean {
  return new RegExp(`(^|[\\s{;])${key}\\s*:`, 'm').test(source)
}

/** D2 reports errors as a JSON list of `{ errmsg }` entries whose messages lead
 *  with the virtual file name (`index:3:5: …`). The line and column are what a
 *  person needs; the file name is ours, not theirs. */
export function formatD2Error(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error)
  try {
    const parsed = JSON.parse(raw) as unknown
    if (Array.isArray(parsed)) {
      const lines = parsed
        .map((entry) =>
          entry && typeof entry === 'object' && typeof (entry as { errmsg?: unknown }).errmsg === 'string'
            ? (entry as { errmsg: string }).errmsg.replace(/^index:/, 'line ')
            : null
        )
        .filter((line): line is string => line !== null)
      if (lines.length > 0) return lines.join('\n')
    }
  } catch {
    /* not the JSON list: show the message as it came */
  }
  return raw
}

let purifier: DOMPurifyInstance | null = null

/** Strip everything from a D2 SVG that could run, fetch or navigate. A
 *  separate instance, so the hooks the Markdown sanitizer installs on the
 *  shared one never apply here and these settings never leak there. */
export function sanitizeD2Svg(svg: string): string {
  purifier ??= DOMPurify(window)
  return purifier.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true, html: true },
    // Markdown labels are XHTML inside a foreignObject.
    ADD_TAGS: ['foreignObject', 'style'],
    // `icon:` and `shape: image` fetch their URL; `link:` navigates. Their
    // contents stay, so a linked shape keeps its label.
    FORBID_TAGS: ['image', 'img', 'a', 'video', 'audio', 'iframe', 'object', 'embed', 'use'],
    FORBID_ATTR: ['href', 'xlink:href', 'src', 'srcset', 'action', 'formaction'],
    // DOMPurify lets HTML into SVG only under `annotation-xml` unless told
    // otherwise, and Chromium then empties every Markdown label (jsdom keeps
    // them, so only the built app shows it). The forbidden tags and attributes
    // above still apply inside the foreignObject.
    HTML_INTEGRATION_POINTS: { foreignobject: true }
  })
}

async function compileAndRender(source: string): Promise<D2RenderResult> {
  const d2 = await loadD2()
  const compiled = await enqueue(() => d2.compile(source))
  const svg = await enqueue(() =>
    d2.render(compiled.diagram, {
      ...compiled.renderOptions,
      noXMLTag: true,
      // Drawn at its own size and shrunk to fit only when wider than the note;
      // D2's default stretches even a two-box diagram to the full width.
      ...(sourceSets(source, 'scale') ? {} : { scale: 1 }),
      // D2 pads for a standalone file (100 px); a diagram in a note sits in a
      // card that pads it already.
      ...(sourceSets(source, 'pad') ? {} : { pad: 16 })
    })
  )
  return { ok: true, svg: sanitizeD2Svg(svg), ownTheme: d2SourceSetsTheme(source) }
}

let idSeq = 0

/** Give one inserted copy of a D2 SVG ids of its own. D2's ids are fixed per
 *  diagram, and Preview and the editor's live widget show the same cached
 *  string: with two copies in the document, `marker-end="url(#mk-…)"` resolves
 *  to the first one, and when that copy sits in the hidden editor every arrow
 *  in Preview loses its head. Call this at each insertion, not on the cache. */
export function withUniqueD2Ids(svg: string): string {
  const suffix = `-z${(idSeq += 1)}`
  const ids = new Set<string>()
  const renamed = svg.replace(/\bid="([^"]+)"/g, (_match, id: string) => {
    ids.add(id)
    return `id="${id}${suffix}"`
  })
  if (ids.size === 0) return svg
  return renamed.replace(/url\(#([^)]+)\)/g, (match, id: string) =>
    ids.has(id) ? `url(#${id}${suffix})` : match
  )
}

/** A finished render, or null when this diagram has not been drawn yet. D2
 *  always renders in its worker, so nothing is drawn synchronously here; this
 *  lets a widget repaint from the cache on re-mount instead of flashing. */
export function peekD2Svg(rawSource: string): D2RenderResult | null {
  return svgCache.get(normalizeSource(rawSource)) ?? null
}

/** Render to a sanitized SVG string, reusing an in-flight render of the same
 *  diagram. Never throws: a broken diagram is a result the caller shows, and
 *  its message quotes the source, so it is shown as text. */
export function renderD2Svg(rawSource: string): Promise<D2RenderResult> {
  const source = normalizeSource(rawSource)
  const cached = svgCache.get(source)
  if (cached) return Promise.resolve(cached)
  const running = inFlight.get(source)
  if (running) return running

  const task = compileAndRender(source).catch(
    (error: unknown): D2RenderResult => ({ ok: false, error: formatD2Error(error) })
  )
  inFlight.set(source, task)
  return task.then((result) => {
    inFlight.delete(source)
    return remember(source, result)
  })
}
