/**
 * The cloud-sync surface of the ZenBridge contract, as this build supports it:
 * not at all.
 *
 * Upstream v2.45 added a hosted companion service (accounts, published notes,
 * a synced vault with conflict resolution, and server-side backups). Reaching
 * it needs an OAuth loopback flow, a sync client and a filesystem adapter in
 * the host process — upstream spends ~90 KB of `apps/desktop/src/main/cloud-*`
 * on it, and none of that exists in Rust yet.
 *
 * So every member here reproduces the WEB bridge's degradation exactly
 * (apps/web/src/bridge/http-bridge.ts): the queries that have an honest empty
 * answer return it, and the mutations reject. `supportsCloudSync` is false in
 * capabilities.ts, so app-core hides the whole feature and none of these should
 * be reached in normal use — they exist so a stray call fails loudly instead of
 * hitting a Tauri command that was never registered.
 *
 * Cloud sync is a deliberate non-goal for a local-first port, not an oversight;
 * see the "Cloud sync" row in GAP-ANALYSIS.md.
 */

import type { ZenBridge } from '@zennotes/bridge-contract/bridge'
import type { CloudAccountStatus } from '@zennotes/bridge-contract/cloud-sync'

function unsupported(name: string): Promise<never> {
  return Promise.reject(
    new Error(`ZenNotes-rs: "${name}" needs the ZenNotes cloud service, which this build has no client for.`)
  )
}

const DISCONNECTED: CloudAccountStatus = { state: 'disconnected', account: null }

type CloudSurface = Pick<
  ZenBridge,
  | 'getCloudAccountStatus'
  | 'connectCloudAccount'
  | 'logoutCloudAccount'
  | 'onCloudAccountChange'
  | 'getCloudServiceAccount'
  | 'listCloudPublishedNotes'
  | 'publishCloudNote'
  | 'updateCloudPublishedNote'
  | 'unpublishCloudNote'
  | 'listCloudVaults'
  | 'getCloudVaultLink'
  | 'linkCloudVault'
  | 'createAndLinkCloudVault'
  | 'unlinkCloudVault'
  | 'deleteCloudVault'
  | 'syncCloudVault'
  | 'getCloudBootstrapConflict'
  | 'resolveCloudBootstrapConflict'
  | 'getCloudConflict'
  | 'saveCloudConflictDraft'
  | 'resolveCloudConflict'
  | 'getCloudSettingsConflict'
  | 'resolveCloudSettingsConflict'
  | 'listCloudBackups'
  | 'getCloudBackupSchedule'
  | 'updateCloudBackupSchedule'
  | 'listCloudBackupItems'
  | 'createCloudBackup'
  | 'downloadCloudBackup'
  | 'deleteCloudBackup'
  | 'restoreCloudBackup'
  | 'restoreCloudBackupNote'
>

export const cloudSyncSurface: CloudSurface = {
  // Account. "Disconnected" is the truthful steady state, so the account
  // panel renders its signed-out view rather than an error.
  getCloudAccountStatus: async () => DISCONNECTED,
  connectCloudAccount: async () => unsupported('connectCloudAccount'),
  logoutCloudAccount: async () => DISCONNECTED,
  onCloudAccountChange: () => () => {},
  getCloudServiceAccount: async () => unsupported('getCloudServiceAccount'),

  // Published notes.
  listCloudPublishedNotes: async () => unsupported('listCloudPublishedNotes'),
  publishCloudNote: async () => unsupported('publishCloudNote'),
  updateCloudPublishedNote: async () => unsupported('updateCloudPublishedNote'),
  unpublishCloudNote: async () => unsupported('unpublishCloudNote'),

  // Synced vaults. `getCloudVaultLink` answers null — this vault is linked to
  // nothing — which is what stops app-core from starting a sync loop at boot.
  listCloudVaults: async () => unsupported('listCloudVaults'),
  getCloudVaultLink: async () => null,
  linkCloudVault: async () => unsupported('linkCloudVault'),
  createAndLinkCloudVault: async () => unsupported('createAndLinkCloudVault'),
  unlinkCloudVault: async () => unsupported('unlinkCloudVault'),
  deleteCloudVault: async () => unsupported('deleteCloudVault'),
  syncCloudVault: async () => unsupported('syncCloudVault'),

  // Conflict resolution. Unreachable without a sync run, but kept complete.
  getCloudBootstrapConflict: async () => unsupported('getCloudBootstrapConflict'),
  resolveCloudBootstrapConflict: async () => unsupported('resolveCloudBootstrapConflict'),
  getCloudConflict: async () => unsupported('getCloudConflict'),
  saveCloudConflictDraft: async () => unsupported('saveCloudConflictDraft'),
  resolveCloudConflict: async () => unsupported('resolveCloudConflict'),
  getCloudSettingsConflict: async () => null,
  resolveCloudSettingsConflict: async () => unsupported('resolveCloudSettingsConflict'),

  // Server-side backups.
  listCloudBackups: async () => unsupported('listCloudBackups'),
  getCloudBackupSchedule: async () => unsupported('getCloudBackupSchedule'),
  updateCloudBackupSchedule: async () => unsupported('updateCloudBackupSchedule'),
  listCloudBackupItems: async () => unsupported('listCloudBackupItems'),
  createCloudBackup: async () => unsupported('createCloudBackup'),
  downloadCloudBackup: async () => unsupported('downloadCloudBackup'),
  deleteCloudBackup: async () => unsupported('deleteCloudBackup'),
  restoreCloudBackup: async () => unsupported('restoreCloudBackup'),
  restoreCloudBackupNote: async () => unsupported('restoreCloudBackupNote')
}
