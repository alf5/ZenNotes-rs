// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'

const tauriInvoke = vi.fn().mockResolvedValue(undefined)
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauriInvoke }))

const { handleAnchorClick } = await import('../bridge/navigation-guard')

function clickOn(href: string, attributes: Record<string, string> = {}, init: MouseEventInit = {}) {
  const anchor = document.createElement('a')
  anchor.setAttribute('href', href)
  for (const [key, value] of Object.entries(attributes)) anchor.setAttribute(key, value)
  const inner = document.createElement('span')
  anchor.append(inner)
  document.body.append(anchor)
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, ...init })
  // jsdom would try to navigate; only the decision is under test.
  let prevented = false
  anchor.addEventListener('click', (e) => {
    handleAnchorClick(e)
    prevented = e.defaultPrevented
    e.preventDefault()
  })
  inner.dispatchEvent(event)
  return prevented
}

describe('main-frame links', () => {
  beforeEach(() => {
    tauriInvoke.mockClear()
    document.body.replaceChildren()
  })

  it('sends an outside link to the OS and stays in the app', () => {
    expect(clickOn('https://example.com/a')).toBe(true)
    expect(tauriInvoke).toHaveBeenCalledWith(
      'app_open_external_url',
      { url: 'https://example.com/a' },
      undefined
    )
  })

  it('opens a vault asset link with its app', () => {
    expect(clickOn('syn-asset://localhost/%2Fv%2Fa.pdf')).toBe(true)
    expect(tauriInvoke).toHaveBeenCalledWith(
      'app_open_vault_asset_url',
      { url: 'syn-asset://localhost/%2Fv%2Fa.pdf' },
      undefined
    )
  })

  it('never leaves for another page of the app or a theme asset', () => {
    expect(clickOn('/other.html')).toBe(true)
    expect(clickOn('zen-theme://soft/a.png')).toBe(true)
    expect(tauriInvoke).not.toHaveBeenCalled()
  })

  it('leaves fragments, downloads and links app-core already took', () => {
    expect(clickOn('#heading')).toBe(false)
    expect(clickOn('blob:http://localhost/1', { download: 'a.png' })).toBe(false)
    expect(clickOn('https://example.com', {}, { button: 1 })).toBe(false)

    const anchor = document.createElement('a')
    anchor.href = 'https://example.com'
    document.body.append(anchor)
    const event = new MouseEvent('click', { bubbles: true, cancelable: true })
    event.preventDefault()
    anchor.addEventListener('click', handleAnchorClick)
    anchor.dispatchEvent(event)
    expect(tauriInvoke).not.toHaveBeenCalled()
  })
})
