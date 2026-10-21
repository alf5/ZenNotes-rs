// Guards #921: the input-method guard (#84, #464) reached only the main
// editor, the floating note and Quick capture. The external-file window, the
// reference pane and the template editor built Vim without it, so a CJK input
// method composed over hjkl and typed `ia` into a file opened from outside the
// vault. Every source that turns Vim on must install the guard too.

import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

describe('vimImeGuard coverage (#921)', () => {
  it('goes with every editor that turns Vim on', () => {
    const src = join(dirname(fileURLToPath(import.meta.url)), '..')
    const sources = readdirSync(src, { recursive: true, encoding: 'utf8' }).filter(
      (file) => /\.tsx?$/.test(file) && !/\.test\.tsx?$/.test(file)
    )
    // A scan that stopped finding the sources would pass on nothing.
    expect(sources.length).toBeGreaterThan(100)
    const withVim = sources.filter((file) => /\bvim\(\)/.test(readFileSync(join(src, file), 'utf8')))
    expect(withVim.length).toBeGreaterThan(3)
    const unguarded = withVim.filter(
      (file) => !/\bvimImeGuard\(/.test(readFileSync(join(src, file), 'utf8'))
    )
    expect(unguarded).toEqual([])
  })
})
