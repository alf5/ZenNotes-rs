import type {
  CloudBackupItemsPage,
  CloudBackupItemsQuery,
  CloudBackupRestore,
  CloudBackupRestoreRequest,
  CloudBackupRestoreResponse,
  CloudBackupRestoreResult,
  CloudBackupSnapshotItemCollection,
  CloudSyncRunSummary,
  CloudSyncVaultCollection
} from '@zennotes/bridge-contract/cloud-sync'
import { formatCloudBytes } from './cloud-bytes'
import { randomUUID } from './random-uuid'

export interface CloudBackupRestoreClient {
  listVaults(): Promise<CloudSyncVaultCollection>
  createBackupRestore(
    vaultId: string,
    backupId: string,
    body: CloudBackupRestoreRequest
  ): Promise<CloudBackupRestoreResponse>
  backupRestore(
    vaultId: string,
    backupId: string,
    restoreId: string
  ): Promise<CloudBackupRestoreResponse>
}

export interface RestoreCloudBackupOptions {
  client: CloudBackupRestoreClient
  vaultId: string
  backupId: string
  sync(): Promise<CloudSyncRunSummary>
  idempotencyKey?: () => string
  wait?: (milliseconds: number) => Promise<void>
  pollIntervalMs?: number
  maxPolls?: number
}

const terminalRestoreStatuses = new Set<CloudBackupRestore['status']>([
  'completed',
  'conflict',
  'failed'
])

/**
 * Restores only when the remote vault is still at the cursor the user saw,
 * then pulls the completed replacement into the local vault.
 */
export async function restoreCloudBackup({
  client,
  vaultId,
  backupId,
  sync,
  idempotencyKey = () => randomUUID(),
  wait = delay,
  pollIntervalMs = 1_000,
  maxPolls = 120
}: RestoreCloudBackupOptions): Promise<CloudBackupRestoreResult> {
  const vault = (await client.listVaults()).data.find((candidate) => candidate.id === vaultId)
  if (!vault) {
    throw new Error('The linked ZenNotes Cloud vault is no longer available.')
  }

  let restore = (
    await client.createBackupRestore(vaultId, backupId, {
      idempotency_key: idempotencyKey(),
      expected_cursor: vault.cursor,
      mode: 'replace'
    })
  ).data

  let polls = 0
  while (!terminalRestoreStatuses.has(restore.status)) {
    if (polls >= maxPolls) {
      throw new Error('The backup restore is still running. Check again in a moment.')
    }
    polls += 1
    await wait(pollIntervalMs)
    restore = (await client.backupRestore(vaultId, backupId, restore.id)).data
  }

  return {
    restore,
    sync: restore.status === 'completed' ? await sync() : null
  }
}

/**
 * A backup's notes as the service paged them. An answer without `meta` has
 * nothing further to page through, so it is one page holding all it listed.
 */
export function cloudBackupItemsPage(
  collection: CloudBackupSnapshotItemCollection,
  query: CloudBackupItemsQuery = {}
): CloudBackupItemsPage {
  const meta = collection.meta
  return {
    items: collection.data,
    page: meta?.current_page ?? 1,
    lastPage: meta?.last_page ?? 1,
    total: meta?.total ?? collection.data.length,
    search: query.search?.trim() ?? ''
  }
}

/**
 * The plan limit a refused backup ran into, with the numbers. The service
 * names the limit and both counts only in the details of BACKUP_QUOTA_EXCEEDED
 * (a refused request, or the failure an automatic backup records); its message
 * names none of them. Null for any other failure and for details this version
 * cannot read, so the service's own message stands.
 */
export function cloudBackupLimitMessage(failure: unknown): string | null {
  if (!failure || typeof failure !== 'object') return null
  const { code, details } = failure as { code?: unknown; details?: unknown }
  if (code !== 'BACKUP_QUOTA_EXCEEDED' || !details || typeof details !== 'object') return null
  const { limit, current, allowed } = details as Record<string, unknown>
  // The service refuses only at or over a limit, so counts below it are not a
  // shape this version can word truthfully.
  if (!isCount(current) || !isCount(allowed) || current < allowed) return null

  switch (limit) {
    case 'max_snapshot_bytes': {
      const most = formatCloudBytes(allowed)
      const size = formatCloudBytes(current)
      // Sizes from 10 MB up print whole: 52.43 MB and 52.42 MB both read "52 MB".
      return `This vault holds ${size === most ? `just over ${most}` : size}, and backups on your plan hold up to ${most}. Remove large files, or contact support to raise the limit.`
    }
    case 'max_snapshot_items':
      return `This vault has ${counted(current, 'file')}, and backups on your plan hold up to ${formatCount(allowed)}. Remove files you no longer need, or contact support to raise the limit.`
    case 'max_snapshots':
      return current === allowed
        ? `This vault already keeps ${counted(current, 'backup')}, the most your plan allows. Delete an older backup to make room.`
        : `This vault keeps ${counted(current, 'backup')}, more than the ${formatCount(allowed)} your plan allows. Delete ${counted(current - allowed + 1, 'older backup')} to make room.`
    default:
      return null
  }
}

/**
 * What to throw when the service refuses a backup. Desktop errors reach the
 * window as message text only, so a plan-limit refusal is reworded here while
 * its details are still attached; any other failure is left as it is.
 */
export function cloudBackupCreateError(error: unknown): unknown {
  const message = cloudBackupLimitMessage(error)
  return message === null ? error : new Error(message, { cause: error })
}

function isCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

function counted(value: number, singular: string): string {
  return `${formatCount(value)} ${value === 1 ? singular : `${singular}s`}`
}

// The sentences are English and sizes print with a "." decimal point, so
// counts group the English way too, whatever locale the host runs in.
function formatCount(value: number): string {
  return value.toLocaleString('en-US')
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds))
}
