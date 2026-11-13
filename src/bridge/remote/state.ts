/**
 * What this window knows about the remote workspace: upstream's preload cache
 * of `RemoteWorkspaceInfo` (apps/desktop/src/preload/index.ts), refreshed after
 * every call that can change the workspace and whenever Rust says it changed.
 *
 * The bridge reads it synchronously, to route each vault call to the server or
 * the local disk and to build asset URLs. If it is ever stale, the worst case
 * is a refusal: while a session is on, Rust has no local vault to touch.
 */

import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { RemoteWorkspaceInfo } from '@zennotes/bridge-contract/ipc'
import { invoke } from '../ipc'

let info: RemoteWorkspaceInfo | null = null

export function remoteWorkspaceInfo(): RemoteWorkspaceInfo | null {
  return info
}

/**
 * A session is live, so vault calls go to the server. A workspace that failed
 * to connect at boot is configured but not connected: Rust reports it with no
 * capabilities and a `bootError`, and nothing can be asked of it.
 */
export function remoteConnected(): boolean {
  return info?.mode === 'remote' && info.capabilities != null
}

export async function refreshRemoteWorkspaceInfo(): Promise<RemoteWorkspaceInfo | null> {
  try {
    info = await invoke<RemoteWorkspaceInfo | null>('workspace_get_info')
  } catch {
    info = null
  }
  return info
}

/** For tests. */
export function setRemoteWorkspaceInfoForTest(next: RemoteWorkspaceInfo | null): void {
  info = next
}

function ownLabel(): string | null {
  try {
    return getCurrentWindow().label
  } catch {
    return null
  }
}

/**
 * Follow the app's one workspace. The window that switched it carries on with
 * its own store; every other window reloads, so a floating note or Quick
 * Capture never keeps showing, or writing to, the workspace that was left.
 */
export function installWorkspaceFollower(): void {
  void refreshRemoteWorkspaceInfo()
  const own = ownLabel()
  void listen<{ origin?: string | null } | null>('workspace://changed', (event) => {
    const origin = event.payload?.origin ?? null
    if (origin && own && origin !== own) {
      window.location.reload()
      return
    }
    void refreshRemoteWorkspaceInfo()
  }).catch(() => {})
}
