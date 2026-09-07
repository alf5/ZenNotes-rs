/**
 * The renderer boot order is a correctness property, not a style choice.
 *
 * app-core's store calls `getConfigSync()` while building its initial state and
 * MEMOIZES the result, so it must not evaluate until `initPortableConfig()` has
 * resolved. ES modules evaluate a module's static imports before its own body,
 * so any static import in `main.tsx` that Rollup packs into the same chunk as
 * that store wins the race — and then config.toml is silently ignored for the
 * entire session (the first-run wizard reappears on every launch, hand-edited
 * settings never apply).
 *
 * That is exactly what happened when `main.tsx` imported `./bridge/tauri-bridge`
 * statically: the bridge pulls shared-domain helpers, which Rollup had grouped
 * into the `app-store` chunk. Nothing in the type system or the bundle output
 * complains, so it is asserted here instead.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const mainSource = readFileSync(resolve(here, '../main.tsx'), 'utf8')

/** Static `import … from '…'` specifiers, ignoring `await import(…)`. */
function staticImports(source: string): string[] {
  return [...source.matchAll(/^\s*import\s[^\n]*?from\s+'([^']+)'/gm)].map((m) => m[1])
}

describe('renderer bootstrap order', () => {
  it('statically imports nothing but the config bootstrap', () => {
    expect(staticImports(mainSource)).toEqual(['./bridge/portable-config'])
  })

  it('awaits the config bootstrap before importing anything else', () => {
    const bootstrap = mainSource.indexOf('await initPortableConfig()')
    expect(bootstrap).toBeGreaterThan(-1)
    // Matched as the import EXPRESSION, not the bare specifier: the comment
    // above the bootstrap names these modules, and a substring search would
    // find the prose rather than the import.
    for (const spec of ['./bridge/tauri-bridge', './bridge/drag-region', '@zennotes/app-core/main']) {
      const at = mainSource.indexOf(`await import('${spec}')`)
      expect(at, `${spec} should be dynamically imported in main.tsx`).toBeGreaterThan(-1)
      expect(at, `${spec} must be imported after the config bootstrap`).toBeGreaterThan(bootstrap)
    }
  })

  it('reaches app-core only through a dynamic import', () => {
    expect(mainSource).toContain("await import('@zennotes/app-core/main')")
    expect(staticImports(mainSource)).not.toContain('@zennotes/app-core/main')
  })
})
