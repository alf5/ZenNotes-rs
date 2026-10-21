import { describe, expect, it } from 'vitest'
import { makeLeaf, type PaneLayout, type PaneLeaf } from './pane-layout'
import { chromeCarriesWindowDrag, isPaneOnTopEdge } from './window-chrome'

function split(direction: 'row' | 'column', children: PaneLayout[]): PaneLayout {
  return {
    kind: 'split',
    id: `split-${direction}`,
    direction,
    children,
    sizes: children.map(() => 1 / children.length)
  }
}

describe('chromeCarriesWindowDrag', () => {
  it('only when a desktop window hides its title bar', () => {
    expect(chromeCarriesWindowDrag(false, 'desktop')).toBe(true)
    expect(chromeCarriesWindowDrag(true, 'desktop')).toBe(false)
  })

  it('never in a browser, whose page cannot move its window', () => {
    expect(chromeCarriesWindowDrag(false, 'web')).toBe(false)
    expect(chromeCarriesWindowDrag(true, 'web')).toBe(false)
  })
})

describe('isPaneOnTopEdge', () => {
  it('a lone pane is on the top edge', () => {
    const only = makeLeaf()
    expect(isPaneOnTopEdge(only, only.id)).toBe(true)
  })

  it('every pane of a side-by-side split is on the top edge', () => {
    const left = makeLeaf()
    const right = makeLeaf()
    const root = split('row', [left, right])
    expect(isPaneOnTopEdge(root, left.id)).toBe(true)
    expect(isPaneOnTopEdge(root, right.id)).toBe(true)
  })

  it('only the first pane of a stacked split is', () => {
    const top = makeLeaf()
    const bottom = makeLeaf()
    const root = split('column', [top, bottom])
    expect(isPaneOnTopEdge(root, top.id)).toBe(true)
    expect(isPaneOnTopEdge(root, bottom.id)).toBe(false)
  })

  it('follows nesting: side by side on top of a stack, stacked under a row', () => {
    const topLeft: PaneLeaf = makeLeaf()
    const topRight: PaneLeaf = makeLeaf()
    const below: PaneLeaf = makeLeaf()
    const rightTop: PaneLeaf = makeLeaf()
    const rightBottom: PaneLeaf = makeLeaf()
    const root = split('row', [
      split('column', [split('row', [topLeft, topRight]), below]),
      split('column', [rightTop, rightBottom])
    ])
    expect(isPaneOnTopEdge(root, topLeft.id)).toBe(true)
    expect(isPaneOnTopEdge(root, topRight.id)).toBe(true)
    expect(isPaneOnTopEdge(root, below.id)).toBe(false)
    expect(isPaneOnTopEdge(root, rightTop.id)).toBe(true)
    expect(isPaneOnTopEdge(root, rightBottom.id)).toBe(false)
  })

  it('an unknown pane is not on the top edge', () => {
    expect(isPaneOnTopEdge(makeLeaf(), 'missing')).toBe(false)
  })
})
