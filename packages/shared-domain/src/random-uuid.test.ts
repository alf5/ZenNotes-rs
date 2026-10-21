import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { CloudSyncIdSource } from './cloud-sync-engine'
import { restoreCloudBackup } from './cloud-backup'
import { CloudSyncHostService } from './cloud-sync-host-service'
import { randomUUID } from './random-uuid'

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

// An iPhone on iOS 15.0 to 15.3: WebKit has crypto.getRandomValues but not
// crypto.randomUUID. Shadow the method for each test, then restore it.
beforeEach(() => {
  Object.defineProperty(crypto, 'randomUUID', { value: undefined, configurable: true, writable: true })
})
afterEach(() => {
  delete (crypto as { randomUUID?: unknown }).randomUUID
})

describe('UUIDs without crypto.randomUUID', () => {
  it('makes distinct version 4 UUIDs from getRandomValues', () => {
    expect(typeof crypto.randomUUID).toBe('undefined')
    const made = new Set(Array.from({ length: 200 }, () => randomUUID()))
    expect(made.size).toBe(200)
    for (const id of made) expect(id).toMatch(UUID_V4)
  })

  it('gives Cloud sync its item and operation ids', () => {
    const service = new CloudSyncHostService({
      persistence: {},
      accountStatus: async () => ({ state: 'disconnected', account: null }),
      createClient: async () => {
        throw new Error('not used here')
      }
    } as unknown as ConstructorParameters<typeof CloudSyncHostService>[0])
    const ids = (service as unknown as { ids: CloudSyncIdSource }).ids
    expect(ids.itemId()).toMatch(UUID_V4)
    expect(ids.operationId()).toMatch(UUID_V4)
  })

  it('restores a backup with an idempotency key it made itself', async () => {
    const createBackupRestore = vi.fn(async () => ({
      data: {
        id: 'restore-1',
        backup_id: 'backup-1',
        mode: 'replace' as const,
        status: 'completed' as const,
        expected_cursor: 3,
        start_cursor: 3,
        end_cursor: 4,
        restored_items: 1,
        deleted_items: 0,
        error: null,
        created_at: '2026-10-12T00:00:00.000Z',
        updated_at: '2026-10-12T00:00:00.000Z'
      }
    }))
    const result = await restoreCloudBackup({
      client: {
        listVaults: async () => ({
          data: [{ id: 'vault-1', name: 'Notes', cursor: 3, created_at: '2026-10-12T00:00:00.000Z', updated_at: '2026-10-12T00:00:00.000Z' }]
        }),
        createBackupRestore,
        backupRestore: vi.fn()
      },
      vaultId: 'vault-1',
      backupId: 'backup-1',
      sync: async () => ({
        cursor: 4,
        pulled: 1,
        pushed: 0,
        conflicts: [],
        bootstrap_conflicts: [],
        local_conflicts: [],
        pending_conflicts: [],
        legacy_conflict_copies: []
      })
    })
    expect(result.restore.status).toBe('completed')
    const body = (createBackupRestore.mock.calls[0] as unknown[])[2] as { idempotency_key: string }
    expect(body.idempotency_key).toMatch(UUID_V4)
  })
})
