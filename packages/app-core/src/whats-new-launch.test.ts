// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'

// The first launch of a version, through the store: what it records, what it
// counts, and what What's New shows (lib/whats-new.ts holds the rules).

beforeEach(() => {
  vi.resetModules()
  localStorage.clear()
})

async function launch(opts: {
  version: string
  prefs?: Record<string, unknown>
  cloud?: 'connected' | 'disconnected'
  hostKind?: 'desktop' | 'ios'
}) {
  if (opts.prefs) localStorage.setItem('zen:prefs:v2', JSON.stringify(opts.prefs))
  const sendUsageCount = vi.fn(async () => {})
  const bridge = {
    getCapabilities: () => ({}),
    getAppInfo: () => ({
      version: opts.version,
      runtime: opts.hostKind === 'ios' ? 'web' : 'desktop',
      hostKind: opts.hostKind ?? 'desktop'
    }),
    getCloudAccountStatus: async () => ({ state: opts.cloud ?? 'disconnected', account: null }),
    sendUsageCount
  }
  Object.defineProperty(window, 'zen', { configurable: true, value: bridge })
  const { useStore } = await import('./store')
  await useStore.getState().showWhatsNewAfterUpdate()
  const state = useStore.getState()
  const saved = JSON.parse(localStorage.getItem('zen:prefs:v2') ?? '{}')
  return { state, saved, sendUsageCount }
}

const returning = { hasCompletedOnboarding: true }

describe('the first launch of a version', () => {
  it('counts a fresh install once and opens nothing', async () => {
    const { state, saved, sendUsageCount } = await launch({ version: '2.68.0' })
    expect(sendUsageCount.mock.calls).toEqual([['install']])
    expect(state.whatsNewOpen).toBe(false)
    expect(saved.lastSeenWhatsNewVersion).toBe('2.68.0')
  })

  it('counts an update and opens What’s New with the Cloud card', async () => {
    const { state, sendUsageCount } = await launch({
      version: '2.68.0',
      prefs: { ...returning, lastSeenWhatsNewVersion: '2.67.0' }
    })
    expect(sendUsageCount.mock.calls).toEqual([['update']])
    expect(state.whatsNewOpen).toBe(true)
    expect(state.whatsNew?.version).toBe('2.68.0')
    expect(state.whatsNew?.entries.map((e) => e.version)).toEqual(['2.68.0'])
    expect(state.whatsNew?.showCloud).toBe(true)
  })

  it('sends nothing with the switch off, and still opens the window', async () => {
    const { state, sendUsageCount } = await launch({
      version: '2.68.0',
      prefs: { ...returning, lastSeenWhatsNewVersion: '2.67.0', usageCount: false }
    })
    expect(sendUsageCount).not.toHaveBeenCalled()
    expect(state.whatsNewOpen).toBe(true)
  })

  it('leaves the Cloud card out for someone on Cloud', async () => {
    const { state } = await launch({
      version: '2.68.0',
      prefs: { ...returning, lastSeenWhatsNewVersion: '2.67.0' },
      cloud: 'connected'
    })
    expect(state.whatsNew?.showCloud).toBe(false)
    expect(state.whatsNew?.entries.length).toBe(1)
  })

  it('opens nothing for someone on Cloud after an update with no highlights', async () => {
    const { state, sendUsageCount } = await launch({
      version: '2.68.1',
      prefs: { ...returning, lastSeenWhatsNewVersion: '2.68.0' },
      cloud: 'connected'
    })
    expect(sendUsageCount.mock.calls).toEqual([['update']])
    expect(state.whatsNewOpen).toBe(false)
  })

  it('does nothing on a relaunch of the same version', async () => {
    const { state, sendUsageCount } = await launch({
      version: '2.68.0',
      prefs: { ...returning, lastSeenWhatsNewVersion: '2.68.0' }
    })
    expect(sendUsageCount).not.toHaveBeenCalled()
    expect(state.whatsNewOpen).toBe(false)
  })

  it('stays out of the phones entirely', async () => {
    const { state, saved, sendUsageCount } = await launch({
      version: '1.24.0',
      prefs: { ...returning, lastSeenWhatsNewVersion: '1.23.0' },
      hostKind: 'ios'
    })
    expect(sendUsageCount).not.toHaveBeenCalled()
    expect(state.whatsNewOpen).toBe(false)
    expect(saved.lastSeenWhatsNewVersion).toBe('1.23.0')
  })
})
