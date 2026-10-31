/**
 * The port's wording swaps for vendored text (tooling/vite/vendored-copy.ts).
 * A re-vendor can reword the text underneath them, so each one is checked
 * against the vendored file, and the swapped text against the built bundle.
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { applyVendoredCopy, VENDORED_COPY } from '../../tooling/vite/vendored-copy'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const assets = join(root, 'dist', 'assets')

describe('vendored text swaps', () => {
  it.each(VENDORED_COPY.map((swap) => [swap.find, swap] as const))(
    'still finds its text once: %s',
    (_find, swap) => {
      const source = readFileSync(join(root, swap.file), 'utf8')
      expect(source.split(swap.find).length - 1).toBe(1)
      expect(applyVendoredCopy(swap.file, source)).toContain(swap.replace)
    }
  )

  it('fails loudly when upstream rewords the text', () => {
    expect(() => applyVendoredCopy(VENDORED_COPY[0]!.file, 'reworded')).toThrow(/Upstream changed/)
  })

  it('leaves other files alone', () => {
    expect(applyVendoredCopy('packages/app-core/src/lib/other.ts', 'same')).toBe('same')
  })

  it.runIf(existsSync(assets))('reaches the built bundle', () => {
    const bundle = readdirSync(assets)
      .filter((name) => name.endsWith('.js'))
      .map((name) => readFileSync(join(assets, name), 'utf8'))
      .join('\n')
    for (const swap of VENDORED_COPY) {
      expect(bundle).not.toContain(swap.find.replace(/^(title|body): /, ''))
    }
    expect(bundle).toContain('then restart ZenNotes-rs.')
    expect(bundle).toContain('ZenNotes-rs does not have this yet.')
  })
})
