import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { isTabStripOverflowing } from './tab-strip-overflow'

const editorPaneSource = readFileSync(
  new URL('../components/EditorPane.tsx', import.meta.url),
  'utf8'
)
const settingsSource = readFileSync(
  new URL('../components/SettingsModal.tsx', import.meta.url),
  'utf8'
)
const storeSource = readFileSync(new URL('../store.ts', import.meta.url), 'utf8')
const stylesSource = readFileSync(new URL('../styles/index.css', import.meta.url), 'utf8')

describe('workspace tab strip overflow styles', () => {
  it('keeps horizontal tab overflow visible without lifting tabs', () => {
    expect(editorPaneSource).toContain('workspace-tab-strip')
    // Flat tabs stay at h-10 and scroll horizontally when overflowing (no lift).
    expect(editorPaneSource).toContain(
      "tabStripOverflowing ? 'overflow-x-auto' : 'overflow-x-hidden'"
    )
    expect(editorPaneSource).toContain('items-stretch')
    expect(stylesSource).toMatch(
      /\.workspace-tab-strip::-webkit-scrollbar\s*\{[^}]*height:\s*6px/s
    )
    expect(stylesSource).not.toMatch(
      /\.workspace-tab-strip::-webkit-scrollbar\s*\{[^}]*display:\s*none/s
    )
    // #421: setting the standard `scrollbar-color`/`scrollbar-width` on the strip
    // makes Chromium ignore the `::-webkit-scrollbar` height above and fall back
    // to a fat ~15px bar, which clipped tab titles in Compact density. Keep the
    // rule element free of those so the slim 6px bar actually renders.
    expect(stylesSource).not.toMatch(
      /\.workspace-tab-strip\s*\{[^}]*scrollbar-(color|width)/s
    )
  })

  it('does not force the no-wrap tab to the strip height, so the scrollbar cannot clip it (#421)', () => {
    // `min-h-8` only belongs in wrap mode (a floor for wrapped rows). In no-wrap
    // the tab must be free to size to the scroll area so the horizontal scrollbar
    // never overlaps the title in Compact density.
    expect(editorPaneSource).toContain("wrapTabs ? 'min-h-8' : ''")
    expect(editorPaneSource).not.toMatch(/h-full min-h-8 min-w-0/)
  })

  it('persists a setting for wrapping tabs onto additional rows', () => {
    expect(storeSource).toContain('wrapTabs: boolean')
    expect(storeSource).toContain('setWrapTabs')
    expect(settingsSource).toContain('Wrap note tabs')
    expect(editorPaneSource).toContain('wrapTabs')
    expect(editorPaneSource).toContain('flex-wrap')
  })

  it('detects horizontal overflow with a small rounding tolerance', () => {
    expect(isTabStripOverflowing({ scrollWidth: 100, clientWidth: 100 })).toBe(false)
    expect(isTabStripOverflowing({ scrollWidth: 100.5, clientWidth: 100 })).toBe(false)
    expect(isTabStripOverflowing({ scrollWidth: 102, clientWidth: 100 })).toBe(true)
  })
})
