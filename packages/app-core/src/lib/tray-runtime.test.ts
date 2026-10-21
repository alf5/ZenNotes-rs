import { describe, expect, it } from 'vitest'
import { traySyncStatusFor } from './tray-runtime'

// #947: the tray shows a linked vault's sync state, and nothing otherwise.
describe('traySyncStatusFor', () => {
  const snapshot = {
    vaultName: 'Work',
    lastSyncedAt: 1_700_000_000_000,
    removedVault: null
  }

  it('reports a linked vault in each of its states', () => {
    expect(traySyncStatusFor({ ...snapshot, phase: 'ready' })).toEqual({
      state: 'idle',
      vaultName: 'Work',
      lastSyncedAt: 1_700_000_000_000
    })
    expect(traySyncStatusFor({ ...snapshot, phase: 'syncing' })?.state).toBe('syncing')
    expect(traySyncStatusFor({ ...snapshot, phase: 'attention' })?.state).toBe('attention')
    expect(traySyncStatusFor({ ...snapshot, phase: 'error' })?.state).toBe('error')
  })

  it('has nothing to say while Cloud is not connected or not linked', () => {
    for (const phase of ['hidden', 'disconnected', 'connecting', 'unlinked'] as const) {
      expect(traySyncStatusFor({ ...snapshot, phase })).toBeNull()
    }
  })

  it('reads a vault that went away as needing the person, as the status bar does', () => {
    expect(
      traySyncStatusFor({ ...snapshot, phase: 'unlinked', removedVault: { vaultName: 'Work', reason: 'deleted' } })
        ?.state
    ).toBe('attention')
  })
})
