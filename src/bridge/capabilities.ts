import type { ZenCapabilities } from '@zennotes/bridge-contract/bridge'

/**
 * Feature flags negotiated with the React frontend. Every flag starts
 * `false` and is flipped on as the matching Rust backend milestone lands
 * (see the migration plan). The UI hides affordances whose capability is
 * off, so the app never calls into an unimplemented command on boot.
 *
 *  - supportsLocalFilesystemPickers — M1 (vault open/pick)
 *  - supportsCustomTemplates        — M10 (templates CRUD)
 *  - supportsFloatingWindows        — M11 (multi-window)
 *  - supportsNativeMenus            — M12 (OS integration)
 *  - supportsRemoteWorkspace        — M14 (remote workspace)
 *  - supportsUpdater                — M15 (auto-updater)
 */
export const TAURI_CAPABILITIES: ZenCapabilities = {
  supportsUpdater: false,
  supportsNativeMenus: false,
  supportsFloatingWindows: true,
  supportsLocalFilesystemPickers: true,
  supportsRemoteWorkspace: false,
  supportsCliInstall: false,
  supportsCustomTemplates: true
}
