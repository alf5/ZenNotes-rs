import { describe, expect, it, vi } from 'vitest'
import type { CloudSyncVault } from '@zennotes/bridge-contract/cloud-sync'
import { cloudBackupCreateError, cloudBackupLimitMessage, restoreCloudBackup } from './cloud-backup'

const vault: CloudSyncVault = {
  id: 'vault-1',
  name: 'Notes',
  cursor: 14,
  created_at: '2026-08-10T12:00:00.000Z',
  updated_at: '2026-08-10T12:00:00.000Z'
}

function restore(status: 'pending' | 'restoring' | 'completed' | 'conflict' | 'failed') {
  return {
    id: 'restore-1',
    backup_id: 'backup-1',
    mode: 'replace' as const,
    status,
    expected_cursor: 14,
    start_cursor: status === 'pending' ? null : 14,
    end_cursor: status === 'completed' ? 18 : null,
    restored_items: status === 'completed' ? 3 : 0,
    deleted_items: status === 'completed' ? 1 : 0,
    error: status === 'failed' ? { code: 'RESTORE_FAILED', message: 'Nope' } : null,
    created_at: '2026-08-10T12:00:00.000Z',
    updated_at: '2026-08-10T12:00:00.000Z'
  }
}

describe('restoreCloudBackup', () => {
  it('uses the latest remote cursor, waits for completion, then syncs locally', async () => {
    const client = {
      listVaults: vi.fn(async () => ({ data: [vault] })),
      createBackupRestore: vi.fn(async () => ({ data: restore('pending') })),
      backupRestore: vi
        .fn()
        .mockResolvedValueOnce({ data: restore('restoring') })
        .mockResolvedValueOnce({ data: restore('completed') })
    }
    const sync = vi.fn(async () => ({
      cursor: 18,
      pulled: 4,
      pushed: 0,
      conflicts: [],
      bootstrap_conflicts: [], local_conflicts: []
    }))

    await expect(
      restoreCloudBackup({
        client,
        vaultId: vault.id,
        backupId: 'backup-1',
        idempotencyKey: () => 'restore-operation-1',
        wait: async () => {},
        sync
      })
    ).resolves.toMatchObject({
      restore: { status: 'completed', end_cursor: 18 },
      sync: { cursor: 18, pulled: 4 }
    })

    expect(client.createBackupRestore).toHaveBeenCalledWith(vault.id, 'backup-1', {
      idempotency_key: 'restore-operation-1',
      expected_cursor: 14,
      mode: 'replace'
    })
    expect(client.backupRestore).toHaveBeenCalledTimes(2)
    expect(sync).toHaveBeenCalledOnce()
  })

  it('returns a restore conflict without overwriting the local vault', async () => {
    const client = {
      listVaults: vi.fn(async () => ({ data: [vault] })),
      createBackupRestore: vi.fn(async () => ({ data: restore('conflict') })),
      backupRestore: vi.fn()
    }
    const sync = vi.fn()

    await expect(
      restoreCloudBackup({
        client,
        vaultId: vault.id,
        backupId: 'backup-1',
        idempotencyKey: () => 'restore-operation-1',
        wait: async () => {},
        sync
      })
    ).resolves.toMatchObject({ restore: { status: 'conflict' }, sync: null })

    expect(client.backupRestore).not.toHaveBeenCalled()
    expect(sync).not.toHaveBeenCalled()
  })

  it('times out without syncing when the restore remains queued', async () => {
    const client = {
      listVaults: vi.fn(async () => ({ data: [vault] })),
      createBackupRestore: vi.fn(async () => ({ data: restore('pending') })),
      backupRestore: vi.fn(async () => ({ data: restore('restoring') }))
    }

    await expect(
      restoreCloudBackup({
        client,
        vaultId: vault.id,
        backupId: 'backup-1',
        idempotencyKey: () => 'restore-operation-1',
        wait: async () => {},
        maxPolls: 2,
        sync: vi.fn()
      })
    ).rejects.toThrow('still running')
  })
})

/** The 409 the service answers a backup with when it would pass a plan limit,
 *  as the desktop and phone request errors both carry it. */
function refused(details: unknown) {
  return Object.assign(new Error('This backup would exceed your plan limits.'), {
    name: 'CloudServiceRequestError',
    status: 409,
    code: 'BACKUP_QUOTA_EXCEEDED',
    details
  })
}

describe('cloudBackupLimitMessage', () => {
  it('names the size limit in the decimal units plans are sold in', () => {
    expect(
      cloudBackupLimitMessage(
        refused({ limit: 'max_snapshot_bytes', current: 54_200_000, allowed: 52_428_800 })
      )
    ).toBe(
      'This vault holds 54 MB, and backups on your plan hold up to 52 MB. Remove large files, or contact support to raise the limit.'
    )
  })

  it('says the vault is just over the size limit when both print the same', () => {
    expect(
      cloudBackupLimitMessage(
        refused({ limit: 'max_snapshot_bytes', current: 52_430_000, allowed: 52_428_800 })
      )
    ).toBe(
      'This vault holds just over 52 MB, and backups on your plan hold up to 52 MB. Remove large files, or contact support to raise the limit.'
    )
  })

  it('names the file limit with grouped counts', () => {
    expect(
      cloudBackupLimitMessage(
        refused({ limit: 'max_snapshot_items', current: 12_345, allowed: 10_000 })
      )
    ).toBe(
      'This vault has 12,345 files, and backups on your plan hold up to 10,000. Remove files you no longer need, or contact support to raise the limit.'
    )
  })

  it('asks for one older backup to go when the vault keeps as many as the plan allows', () => {
    expect(
      cloudBackupLimitMessage(refused({ limit: 'max_snapshots', current: 30, allowed: 30 }))
    ).toBe(
      'This vault already keeps 30 backups, the most your plan allows. Delete an older backup to make room.'
    )
    expect(
      cloudBackupLimitMessage(refused({ limit: 'max_snapshots', current: 1, allowed: 1 }))
    ).toBe(
      'This vault already keeps 1 backup, the most your plan allows. Delete an older backup to make room.'
    )
  })

  it('counts the backups to delete when the vault keeps more than a lowered limit', () => {
    expect(
      cloudBackupLimitMessage(refused({ limit: 'max_snapshots', current: 35, allowed: 30 }))
    ).toBe(
      'This vault keeps 35 backups, more than the 30 your plan allows. Delete 6 older backups to make room.'
    )
  })

  it('reads the failure an automatic backup records the same way', () => {
    expect(
      cloudBackupLimitMessage({
        code: 'BACKUP_QUOTA_EXCEEDED',
        message: 'The latest automatic backup was skipped because this vault reached a backup limit.',
        details: { limit: 'max_snapshot_items', current: 10_001, allowed: 10_000 },
        occurred_at: '2026-10-05T03:15:00+00:00'
      })
    ).toBe(
      'This vault has 10,001 files, and backups on your plan hold up to 10,000. Remove files you no longer need, or contact support to raise the limit.'
    )
  })

  it.each([
    [
      'another refusal',
      Object.assign(new Error('Upgrade required.'), {
        status: 403,
        code: 'FEATURE_NOT_ENTITLED',
        details: { limit: 'max_snapshots', current: 30, allowed: 30 }
      })
    ],
    ['a limit this version does not know', refused({ limit: 'max_archive_bytes', current: 9, allowed: 8 })],
    ['a refusal without details', refused(null)],
    ['details sent as a list', refused(['max_snapshots', 30, 30])],
    ['counts sent as text', refused({ limit: 'max_snapshots', current: '30', allowed: '30' })],
    ['fractional counts', refused({ limit: 'max_snapshot_items', current: 12.5, allowed: 10 })],
    ['a vault under the limit it names', refused({ limit: 'max_snapshot_bytes', current: 1_000, allowed: 52_428_800 })],
    ['a network failure', new TypeError('fetch failed')],
    ['nothing at all', null]
  ])('leaves the service message standing for %s', (_case, failure) => {
    expect(cloudBackupLimitMessage(failure)).toBeNull()
  })
})

describe('cloudBackupCreateError', () => {
  it('rewords a plan-limit refusal as the text Electron carries across IPC, keeping the cause', () => {
    const refusal = refused({ limit: 'max_snapshots', current: 30, allowed: 30 })
    const thrown = cloudBackupCreateError(refusal)

    // ipcMain.handle sends a rejection to the window as String(error).
    expect(String(thrown)).toBe(
      'Error: This vault already keeps 30 backups, the most your plan allows. Delete an older backup to make room.'
    )
    expect((thrown as Error).cause).toBe(refusal)
  })

  it('returns any other failure untouched', () => {
    const outage = Object.assign(new Error('ZenNotes Cloud request failed (503).'), {
      name: 'CloudServiceRequestError',
      status: 503,
      code: null,
      details: null
    })
    const unknownLimit = refused({ limit: 'max_archive_bytes', current: 9, allowed: 8 })

    expect(cloudBackupCreateError(outage)).toBe(outage)
    expect(cloudBackupCreateError(unknownLimit)).toBe(unknownLimit)
  })
})
