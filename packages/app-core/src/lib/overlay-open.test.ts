// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest'
import { isAppOverlayOpen, isOverlayOrDialogOpen } from './overlay-open'

afterEach(() => {
  document.body.innerHTML = ''
})

describe('isAppOverlayOpen', () => {
  it('is false with no overlay in the DOM', () => {
    expect(isAppOverlayOpen()).toBe(false)
  })

  it('is true while a prompt/confirm modal is open', () => {
    const el = document.createElement('div')
    el.setAttribute('data-prompt-modal', '')
    document.body.appendChild(el)
    expect(isAppOverlayOpen()).toBe(true)
  })

  it('is true while a context menu is open', () => {
    const el = document.createElement('div')
    el.setAttribute('data-ctx-menu', '')
    document.body.appendChild(el)
    expect(isAppOverlayOpen()).toBe(true)
  })

  it('is true while the Cloud conflict queue is open', () => {
    const el = document.createElement('div')
    el.setAttribute('data-cloud-conflict-dialog', '')
    document.body.appendChild(el)
    expect(isAppOverlayOpen()).toBe(true)
  })
})

describe('isOverlayOrDialogOpen', () => {
  const open = (attrs: Record<string, string>): void => {
    const el = document.createElement('div')
    for (const [name, value] of Object.entries(attrs)) el.setAttribute(name, value)
    document.body.appendChild(el)
  }

  it('is false with nothing open', () => {
    expect(isOverlayOrDialogOpen()).toBe(false)
  })

  it('covers the context menus and prompts isAppOverlayOpen covers', () => {
    open({ 'data-ctx-menu': '' })
    expect(isOverlayOrDialogOpen()).toBe(true)
  })

  it('is true while Settings is open, which isAppOverlayOpen misses', () => {
    open({ role: 'dialog', 'aria-modal': 'true', 'data-settings-dialog': '' })
    expect(isAppOverlayOpen()).toBe(false)
    expect(isOverlayOrDialogOpen()).toBe(true)
  })

  it('is true while a shared Modal dialog (a palette, a picker) is open', () => {
    open({ role: 'dialog', 'aria-modal': 'true' })
    expect(isOverlayOrDialogOpen()).toBe(true)
  })
})
