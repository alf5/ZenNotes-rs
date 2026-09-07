/**
 * The boot-order guard, asserted against the BUILT BUNDLE.
 *
 * `main-bootstrap-order.test.ts` pins the source shape, but the bug this guards
 * against lived entirely in Rollup's chunking and left the source looking
 * correct: `src/bridge/tauri-bridge.ts` was imported dynamically, yet its chunk
 * statically imported the `app-store` chunk (they shared the vendored
 * shared-domain helpers). A dynamic import evaluates its chunk's static
 * dependencies first, so app-core's store — and its memoizing `loadPrefs()` —
 * ran before `createTauriBridge()` had assigned `window.zen`. The store saw no
 * bridge, cached the defaults, and config.toml was ignored for the whole
 * session: hand-edited settings never applied and the first-run wizard
 * reappeared on every launch.
 *
 * Nothing in the source or the type system shows that, so the rule is stated
 * here in the only place it is observable: neither the entry chunk nor the
 * bridge chunk may statically reach the store.
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '../../dist')
const assets = join(dist, 'assets')
const built = existsSync(join(dist, 'index.html')) && existsSync(assets)

/** Chunk specifiers a chunk imports STATICALLY (`from"./x.js"`), not `import("./x.js")`. */
function staticChunkImports(file: string): string[] {
  const code = readFileSync(file, 'utf8')
  return [...code.matchAll(/from"(\.\/[^"]+)"/g)].map((m) => m[1])
}

function chunkStartingWith(prefix: string): string {
  const name = readdirSync(assets).find((f) => f.startsWith(prefix) && f.endsWith('.js'))
  expect(name, `no ${prefix}* chunk in dist/assets`).toBeTruthy()
  return join(assets, name as string)
}

describe.skipIf(!built)('built bundle boot order', () => {
  it('the entry statically imports only the config bootstrap', () => {
    const html = readFileSync(join(dist, 'index.html'), 'utf8')
    const entry = /assets\/(index-[A-Za-z0-9_-]+\.js)/.exec(html)?.[1]
    expect(entry, 'no entry chunk referenced from index.html').toBeTruthy()
    const imports = staticChunkImports(join(assets, entry as string))
    expect(imports.every((i) => i.includes('bootstrap-config')), `entry imports ${imports}`).toBe(
      true
    )
  })

  it('the bridge chunk does not statically reach app-core’s store', () => {
    const imports = staticChunkImports(chunkStartingWith('tauri-bridge-'))
    expect(imports.filter((i) => i.includes('app-store'))).toEqual([])
  })

  it('keeps the vendored packages in chunks of their own', () => {
    // What makes the rule above hold rather than being a coincidence.
    for (const prefix of ['shared-domain-', 'bridge-contract-', 'bootstrap-config-']) {
      expect(
        readdirSync(assets).some((f) => f.startsWith(prefix) && f.endsWith('.js')),
        `expected a ${prefix}* chunk`
      ).toBe(true)
    }
  })
})
