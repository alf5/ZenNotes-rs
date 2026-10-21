import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const stylesSource = readFileSync(new URL('../styles/index.css', import.meta.url), 'utf8').replace(
  /\/\*[\s\S]*?\*\//g,
  ''
)

// Every `::-webkit-scrollbar...` rule body in the stylesheet, keyed by selector.
function scrollbarRules(): Array<{ selector: string; body: string }> {
  return [...stylesSource.matchAll(/([^{}]*::-webkit-scrollbar[^{}]*)\{([^}]*)\}/g)].map((match) => ({
    selector: match[1].trim(),
    body: match[2]
  }))
}

describe('scrollbar styles', () => {
  it('never sets opacity on a scrollbar part, which Chromium ignores', () => {
    // The sidebar thumb once asked for `opacity: 0.3` and was painted at full
    // text colour, because scrollbar pseudo-elements do not take opacity.
    const withOpacity = scrollbarRules().filter((rule) => /(^|[\s;])opacity\s*:/.test(rule.body))
    expect(withOpacity.map((rule) => rule.selector)).toEqual([])
  })

  it('tints the sidebar thumb with its text colour at 30%, and 50% under the pointer (#451)', () => {
    const rules = scrollbarRules()
    const thumb = rules.find((rule) => rule.selector === '.zn-sidebar-scroll::-webkit-scrollbar-thumb')
    const hover = rules.find((rule) => rule.selector === '.zn-sidebar-scroll::-webkit-scrollbar-thumb:hover')
    expect(thumb?.body).toMatch(/background:\s*color-mix\(in srgb, currentColor 30%, transparent\)/)
    expect(hover?.body).toMatch(/background:\s*color-mix\(in srgb, currentColor 50%, transparent\)/)
  })
})
