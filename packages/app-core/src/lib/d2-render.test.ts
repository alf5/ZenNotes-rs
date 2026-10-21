// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'

// The real runtime is an 8 MB WebAssembly build; these tests drive a stand-in
// with the same one-callback-per-worker contract, which is what the queue in
// d2-render exists for.
const runtime = vi.hoisted(() => {
  const calls: { kind: 'compile' | 'render'; arg: unknown }[] = []
  let pending: ((value: unknown) => void) | null = null
  const settle = (value: unknown): Promise<unknown> =>
    new Promise((resolve) => {
      // Like the wrapper: a call made while another is pending replaces its
      // callback, so an unqueued second call strands the first.
      pending = resolve
      setTimeout(() => {
        if (pending === resolve) {
          pending = null
          resolve(value)
        }
      }, 5)
    })
  class D2 {
    compile(source: string): Promise<unknown> {
      calls.push({ kind: 'compile', arg: source })
      if (source.includes('->-')) return Promise.reject(new Error(JSON.stringify([{ errmsg: 'index:2:3: unexpected text' }])))
      return settle({ diagram: { source }, renderOptions: { pad: 100, themeID: 0 } })
    }
    render(diagram: { source: string }, options: unknown): Promise<unknown> {
      calls.push({ kind: 'render', arg: options })
      return settle(
        `<svg class="d2-1 d2-svg"><style>.d2-1 .fill-N1{fill:#0A0F25;}</style><text class="fill-N1">${diagram.source}</text>` +
          '<image href="https://example.com/icon.svg"/><foreignObject><div xmlns="http://www.w3.org/1999/xhtml"><img src="x" onerror="alert(1)"/><b onclick="x()">label</b></div></foreignObject></svg>'
      )
    }
  }
  return { calls, D2 }
})

vi.mock('./d2-runtime', () => ({ D2: runtime.D2, createD2: async () => new runtime.D2() }))

import { d2SourceSetsTheme, formatD2Error, peekD2Svg, renderD2Svg, sanitizeD2Svg, withUniqueD2Ids } from './d2-render'

beforeEach(() => {
  runtime.calls.length = 0
})

describe('renderD2Svg', () => {
  it('queues concurrent diagrams so neither is stranded by the shared worker callback', async () => {
    const [a, b] = await Promise.all([renderD2Svg('a -> b'), renderD2Svg('c -> d')])
    expect(a.ok && a.svg).toContain('a -&gt; b')
    expect(b.ok && b.svg).toContain('c -&gt; d')
  })

  it('draws at natural size with a small pad unless the diagram sets its own', async () => {
    await renderD2Svg('x -> y')
    expect(runtime.calls.find((c) => c.kind === 'render')?.arg).toMatchObject({ noXMLTag: true, scale: 1, pad: 16 })
    runtime.calls.length = 0
    await renderD2Svg('vars: { d2-config: { pad: 40 } }\nx -> z')
    const options = runtime.calls.find((c) => c.kind === 'render')?.arg as Record<string, unknown>
    expect(options.pad).toBe(100)
    expect(options.scale).toBe(1)
  })

  it('caches by source, so the editor and Preview share one drawing', async () => {
    await renderD2Svg('p -> q\n')
    runtime.calls.length = 0
    const again = await renderD2Svg('  p -> q')
    expect(runtime.calls).toHaveLength(0)
    expect(peekD2Svg('p -> q')).toEqual(again)
  })

  it('reports a broken diagram as text with its line and column', async () => {
    const result = await renderD2Svg('a ->- b')
    expect(result).toEqual({ ok: false, error: 'line 2:3: unexpected text' })
  })

  it('never hands the DOM anything that runs, fetches or navigates', async () => {
    const result = await renderD2Svg('safe -> label')
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.svg).not.toMatch(/onerror|onclick|<image|<img|https:\/\/example/)
    expect(result.svg).toContain('label')
    expect(result.svg).toContain('.fill-N1')
  })
})

describe('d2SourceSetsTheme', () => {
  it('lets a diagram that picks a D2 theme keep it', () => {
    expect(d2SourceSetsTheme('vars: {\n  d2-config: {\n    theme-id: 300\n  }\n}\na -> b')).toBe(true)
    expect(d2SourceSetsTheme('vars: { d2-config: { dark-theme-id: 200 } }')).toBe(true)
    expect(d2SourceSetsTheme('a -> b: theme-id is just words')).toBe(false)
    expect(d2SourceSetsTheme('a -> b')).toBe(false)
  })
})

describe('formatD2Error', () => {
  it('turns the JSON error list into lines', () => {
    expect(
      formatD2Error(new Error('[{"range":"index,0:0:0-0:4:4","errmsg":"index:1:1: connection missing destination"},{"errmsg":"index:4:2: bad"}]'))
    ).toBe('line 1:1: connection missing destination\nline 4:2: bad')
  })

  it('keeps any other message as it came', () => {
    expect(formatD2Error(new Error('worker crashed'))).toBe('worker crashed')
  })
})

describe('sanitizeD2Svg', () => {
  it('keeps the stylesheet and labels while removing links', () => {
    const out = sanitizeD2Svg(
      '<svg xmlns="http://www.w3.org/2000/svg" class="d2-svg"><style type="text/css"><![CDATA[.d2-9 .fill-B1{fill:#0D32B2;}]]></style>' +
        '<a href="javascript:alert(1)" xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="https://example.com"><text class="fill-B1">Server</text></a></svg>'
    )
    expect(out).toContain('.fill-B1')
    expect(out).toContain('Server')
    expect(out).not.toMatch(/<a[\s>]|javascript:|https:\/\/example/)
  })
})

describe('withUniqueD2Ids', () => {
  it('gives each inserted copy its own ids, references included', () => {
    const svg = '<svg><marker id="mk-1"/><mask id="d2-7"/><path marker-end="url(#mk-1)" mask="url(#d2-7)" fill="url(#elsewhere)"/></svg>'
    const a = withUniqueD2Ids(svg)
    const b = withUniqueD2Ids(svg)
    const idsOf = (s: string) => [...s.matchAll(/id="([^"]+)"/g)].map((m) => m[1])
    expect(idsOf(a)).not.toEqual(idsOf(b))
    for (const copy of [a, b]) {
      const [marker, mask] = idsOf(copy)
      expect(copy).toContain(`marker-end="url(#${marker})"`)
      expect(copy).toContain(`mask="url(#${mask})"`)
      // A reference to an id this SVG does not define is left alone.
      expect(copy).toContain('url(#elsewhere)')
    }
  })
})
