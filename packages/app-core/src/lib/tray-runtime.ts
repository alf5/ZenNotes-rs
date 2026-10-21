import type { TraySyncStatus } from '@shared/ipc'
import { create } from 'zustand'
import { useCloudSyncStatusStore, type CloudSyncPhase } from './cloud-auto-sync'

/**
 * The renderer's side of "Keep running in the tray" (#947). The setting lives
 * in the main process, which needs it before any window exists; this mirrors
 * it so Settings, the palette and the Vim leader can read it synchronously.
 */
interface TrayState {
  keepRunningInTray: boolean
}

export const useTrayStore = create<TrayState>(() => ({ keepRunningInTray: false }))

export async function loadKeepRunningInTray(): Promise<void> {
  const enabled = await window.zen.getKeepRunningInTray?.()
  if (typeof enabled === 'boolean') useTrayStore.setState({ keepRunningInTray: enabled })
}

export async function setKeepRunningInTray(enabled: boolean): Promise<void> {
  const applied = await window.zen.setKeepRunningInTray?.(enabled)
  if (typeof applied === 'boolean') useTrayStore.setState({ keepRunningInTray: applied })
}

/** Hide this window to the tray. Main refuses while the tray is off, since a
 *  hidden window would then have no way back. */
export async function hideWindowToTray(): Promise<boolean> {
  if (!useTrayStore.getState().keepRunningInTray) return false
  return (await window.zen.hideWindowToTray?.()) ?? false
}

interface CloudSyncSnapshot {
  phase: CloudSyncPhase
  vaultName: string | null
  lastSyncedAt: number | null
  removedVault: unknown
}

/** What the tray shows for this window's Cloud sync. Only a linked vault has
 *  a state to report; signed out, connecting or not linked reads as nothing,
 *  except a vault that went away, which needs the person like the status bar
 *  says it does. */
export function traySyncStatusFor(snapshot: CloudSyncSnapshot): TraySyncStatus | null {
  const base = { vaultName: snapshot.vaultName, lastSyncedAt: snapshot.lastSyncedAt }
  switch (snapshot.phase) {
    case 'ready':
      return { state: 'idle', ...base }
    case 'syncing':
      return { state: 'syncing', ...base }
    case 'attention':
      return { state: 'attention', ...base }
    case 'error':
      return { state: 'error', ...base }
    case 'unlinked':
      return snapshot.removedVault ? { state: 'attention', ...base } : null
    default:
      return null
  }
}

/** Report this window's sync state to the tray whenever it changes. */
export function startTraySyncStatusReporter(): () => void {
  if (!window.zen.reportTraySyncStatus) return () => undefined
  let last: string | null = null
  const send = (): void => {
    const status = traySyncStatusFor(useCloudSyncStatusStore.getState())
    const key = JSON.stringify(status)
    if (key === last) return
    last = key
    window.zen.reportTraySyncStatus?.(status)
  }
  send()
  return useCloudSyncStatusStore.subscribe(send)
}
