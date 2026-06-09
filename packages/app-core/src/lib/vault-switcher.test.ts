import { describe, expect, it } from 'vitest'
import { buildVaultSwitcherEntries } from './vault-switcher'

describe('buildVaultSwitcherEntries', () => {
  it('keeps the current local vault and recent local vaults in the switcher list', () => {
    const entries = buildVaultSwitcherEntries({
      localVaults: [
        { root: '/vaults/work', name: 'Work', lastOpenedAt: 2 },
        { root: '/vaults/personal', name: 'Personal', lastOpenedAt: 3 }
      ],
      remoteProfiles: [],
      currentVault: { root: '/vaults/personal', name: 'Personal' },
      workspaceMode: 'local',
      remoteWorkspaceInfo: null
    })

    expect(entries).toEqual([
      {
        kind: 'local',
        root: '/vaults/personal',
        name: 'Personal',
        lastOpenedAt: Number.MAX_SAFE_INTEGER,
        current: true,
        location: '/vaults/personal'
      },
      {
        kind: 'local',
        root: '/vaults/work',
        name: 'Work',
        lastOpenedAt: 2,
        current: false,
        location: '/vaults/work'
      }
    ])
  })

  it('adds the current local vault even when it is not in the recent list', () => {
    const entries = buildVaultSwitcherEntries({
      localVaults: [{ root: '/vaults/work', name: 'Work', lastOpenedAt: 2 }],
      remoteProfiles: [],
      currentVault: { root: '/vaults/current', name: 'Current' },
      workspaceMode: 'local',
      remoteWorkspaceInfo: null
    })

    expect(entries.map((entry) => [entry.kind, entry.name, entry.current])).toEqual([
      ['local', 'Current', true],
      ['local', 'Work', false]
    ])
  })

  it('includes saved remote profiles alongside local vaults', () => {
    const entries = buildVaultSwitcherEntries({
      localVaults: [{ root: '/vaults/work', name: 'Work', lastOpenedAt: 2 }],
      remoteProfiles: [
        {
          id: 'remote-1',
          name: 'Server Notes',
          baseUrl: 'https://notes.example.com',
          hasCredential: true,
          vaultPath: '/team',
          lastConnectedAt: 5
        }
      ],
      currentVault: { root: '/vaults/work', name: 'Work' },
      workspaceMode: 'local',
      remoteWorkspaceInfo: null
    })

    expect(entries.map((entry) => [entry.kind, entry.name, entry.current, entry.location])).toEqual([
      ['local', 'Work', true, '/vaults/work'],
      ['remote', 'Server Notes', false, 'https://notes.example.com /team']
    ])
  })

  it('marks the current remote profile and keeps it at the top', () => {
    const entries = buildVaultSwitcherEntries({
      localVaults: [{ root: '/vaults/work', name: 'Work', lastOpenedAt: 8 }],
      remoteProfiles: [
        {
          id: 'remote-old',
          name: 'Archive Server',
          baseUrl: 'https://archive.example.com',
          hasCredential: true,
          vaultPath: null,
          lastConnectedAt: 10
        },
        {
          id: 'remote-current',
          name: 'Team Server',
          baseUrl: 'https://team.example.com',
          hasCredential: true,
          vaultPath: '/team',
          lastConnectedAt: 4
        }
      ],
      currentVault: { root: '/team', name: 'Team' },
      workspaceMode: 'remote',
      remoteWorkspaceInfo: {
        mode: 'remote',
        baseUrl: 'https://team.example.com',
        authConfigured: true,
        capabilities: null,
        profileId: 'remote-current'
      }
    })

    expect(entries.map((entry) => [entry.kind, entry.name, entry.current])).toEqual([
      ['remote', 'Team Server', true],
      ['remote', 'Archive Server', false],
      ['local', 'Work', false]
    ])
  })
})
