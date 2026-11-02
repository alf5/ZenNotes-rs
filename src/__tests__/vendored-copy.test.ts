/**
 * The port's wording swaps for vendored text (tooling/vite/vendored-copy.ts).
 * A re-vendor can reword the text underneath them, so each one is checked
 * against the vendored file, and the swapped text against the built bundle.
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { hideHelpEntries } from '../../tooling/vite/help-hide'
import {
  applyVendoredCopy,
  HELP_HIDDEN,
  staleHelpEntries,
  VENDORED_COPY
} from '../../tooling/vite/vendored-copy'

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

  it('hides only Help entries that still exist', () => {
    const help = readFileSync(join(root, 'packages/app-core/src/lib/help.ts'), 'utf8')
    expect(staleHelpEntries(help)).toEqual([])
    expect(staleHelpEntries(help.replace("label: 'Updates and releases'", "label: 'Reworded'"))).toEqual([
      'Updates and releases'
    ])
    expect(() => applyVendoredCopy('packages/app-core/src/lib/help.ts', 'reworded')).toThrow(/Upstream changed/)
  })

  it('removes hidden Help entries from the lists themselves', () => {
    const cards = [{ title: 'Search from Raycast on macOS' }, { title: 'Kept' }]
    const cli = [{ title: 'What the CLI is for' }]
    const settings = [
      { title: 'CLI', items: [] },
      { title: 'About', items: [{ label: 'Updates and releases' }, { label: 'App identity' }] }
    ]
    hideHelpEntries({ cards: [cards], settings, clearCards: [cli] }, HELP_HIDDEN)
    expect(cards).toEqual([{ title: 'Kept' }])
    expect(cli).toEqual([])
    expect(settings).toEqual([{ title: 'About', items: [{ label: 'App identity' }] }])
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
      // A swap that only adds to the sentence still contains it.
      if (swap.replace.includes(swap.find)) continue
      expect(bundle).not.toContain(swap.find.replace(/^(title|body): /, ''))
    }
    expect(bundle).toContain('then restart ZenNotes-rs.')
    expect(bundle).toContain('ZenNotes-rs does not have this yet.')
    expect(bundle).toContain('run as zennotes-rs mcp')
    expect(bundle).not.toContain('packaged Electron binary')
    expect(bundle).not.toContain('the Electron or browser engine')
  })
})
