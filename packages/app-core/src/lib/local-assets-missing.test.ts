// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from 'vitest'

// A file over Cloud's per-file limit never leaves the device that has it, so
// every other device gets the note and its `![[clip.mp4]]` with no file behind
// it. The vault's file listing is what tells such an embed apart from one whose
// file is merely not listed yet.

function installZen(): void {
  Object.defineProperty(window, 'zen', {
    configurable: true,
    value: {
      resolveLocalAssetUrl: vi.fn((_r: string, _n: string, href: string) => `zen-asset://note/${href}`),
      resolveVaultAssetUrl: vi.fn((_r: string, rel: string) => `zen-asset://vault/${rel}`)
    }
  })
}

async function load() {
  vi.resetModules()
  localStorage.clear()
  installZen()
  const { useStore } = await import('../store')
  const { resolveLocalAsset, enhanceLocalAssetNodes } = await import('./local-assets')
  return { useStore, resolveLocalAsset, enhanceLocalAssetNodes }
}

beforeEach(() => {
  vi.restoreAllMocks()
})

const NOTE = 'inbox/Note.md'

describe('resolveLocalAsset', () => {
  it('names the listed file an href answers to, and its URL', async () => {
    const { useStore, resolveLocalAsset } = await load()
    useStore.setState({ assetFiles: [{ path: 'assets/clip.mp4' }] as never, assetFilesListed: true })
    expect(resolveLocalAsset('/v', NOTE, 'assets/clip.mp4')).toEqual({
      url: 'zen-asset://vault/assets/clip.mp4',
      path: 'assets/clip.mp4',
      missing: false
    })
  })

  it('calls the file missing when the listing read from the vault has nothing that answers', async () => {
    const { useStore, resolveLocalAsset } = await load()
    useStore.setState({ assetFiles: [{ path: 'assets/clip.mp4' }] as never, assetFilesListed: true })
    expect(resolveLocalAsset('/v', NOTE, 'assets/gone.mp4')).toEqual({
      url: 'zen-asset://note/assets/gone.mp4',
      path: null,
      missing: true
    })
  })

  it('calls the file missing in a vault whose listing holds no files at all', async () => {
    const { useStore, resolveLocalAsset } = await load()
    useStore.setState({ assetFiles: [], assetFilesListed: true })
    expect(resolveLocalAsset('/v', NOTE, 'assets/gone.mp4')?.missing).toBe(true)
  })

  it('does not call a file missing when the href cannot choose between two listed files of that name', async () => {
    const { useStore, resolveLocalAsset } = await load()
    useStore.setState({
      assetFiles: [{ path: 'Projects/A/clip.mp4' }, { path: 'Projects/B/clip.mp4' }] as never,
      assetFilesListed: true
    })
    expect(resolveLocalAsset('/v', NOTE, 'clip.mp4')).toEqual({
      url: 'zen-asset://note/clip.mp4',
      path: null,
      missing: false
    })
  })

  it('decides nothing before the first listing lands', async () => {
    const { useStore, resolveLocalAsset } = await load()
    useStore.setState({ assetFiles: [], assetFilesListed: false })
    expect(resolveLocalAsset('/v', NOTE, 'assets/gone.mp4')).toBeNull()
  })

  it('never calls a file missing from a list seeded by hand, and keeps the guessed URL', async () => {
    // The share viewer and PDF export seed `assetFiles` themselves; the share
    // viewer serves files its list leaves out from the guessed URL.
    const { useStore, resolveLocalAsset } = await load()
    useStore.setState({ assetFiles: [{ path: 'assets/clip.mp4' }] as never, assetFilesListed: false })
    expect(resolveLocalAsset('/v', NOTE, 'assets/gone.mp4')).toEqual({
      url: 'zen-asset://note/assets/gone.mp4',
      path: null,
      missing: false
    })
  })
})

describe('enhanceLocalAssetNodes: a missing file', () => {
  it('says so where an embed on its own line would be, and leaves a picture inside a sentence alone', async () => {
    const { useStore, enhanceLocalAssetNodes } = await load()
    useStore.setState({ assetFiles: [{ path: 'assets/clip.mp4' }] as never, assetFilesListed: true })
    const root = document.createElement('div')
    root.innerHTML = [
      '<p data-source-line="1"><img src="assets/gone.png" alt="A chart"></p>',
      '<p data-source-line="3">See <img src="assets/inline.png" alt="inline"> here.</p>'
    ].join('')

    enhanceLocalAssetNodes(root, { vaultRoot: '/v', notePath: NOTE })

    const card = root.querySelector<HTMLElement>('[data-local-asset-missing]')!
    expect(card.dataset.localAssetMissing).toBe('image')
    expect(card.dataset.localAssetHref).toBe('assets/gone.png')
    expect(card.dataset.sourceLine).toBe('1')
    // Not tagged as an asset: the asset menu has no file to act on, and a
    // double-click on the card should edit its line like any block.
    expect(card.dataset.localAssetKind).toBeUndefined()
    expect(card.dataset.localAssetUrl).toBeUndefined()
    expect(card.querySelector('[role="note"]')?.textContent).toBe("gone.png isn't on this device.")
    expect(root.querySelectorAll('[data-local-asset-missing]')).toHaveLength(1)
    expect(root.querySelector<HTMLImageElement>('img[alt="inline"]')?.dataset.localAssetUrl).toBe(
      'zen-asset://note/assets/inline.png'
    )
  })
})
