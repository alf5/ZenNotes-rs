/**
 * `src/bridge/portable-config.ts` is the PORT's own file, not vendored — it is
 * upstream's `apps/desktop/src/main/app-config.ts` TOML layer, copied. So a
 * re-vendor does not update it, and it silently rots: every portable pref
 * upstream adds keeps working in the app (localStorage) while quietly never
 * reaching config.toml, which breaks the one promise that file makes — sync
 * your settings across machines with git/stow/chezmoi.
 *
 * That is exactly what happened across v2.15 → v2.45: 21 of 79 portable prefs
 * had no TOML mapping, `workflowsEnabled` among them, so the Workflows feature
 * could not be switched on from the config file at all. v2.45 → v2.73 would
 * have left 11 more behind; this test caught them.
 *
 * This test states the invariant instead of relying on someone remembering:
 * every key in shared-domain's PORTABLE_PREF_KEYS (which IS vendored, so it
 * updates itself) must have a mapping here.
 */
import { describe, expect, it } from 'vitest'
import { PORTABLE_PREF_KEYS } from '@shared/app-config'
import { portableConfigMappedKeys } from '../bridge/portable-config'

describe('portable config TOML mappings', () => {
  it('covers every portable pref upstream defines', () => {
    const mapped = portableConfigMappedKeys()
    // Upstream maps every portable pref since v2.52 (keepViewModeAcrossNotes
    // was the last exception), so there is nothing left to exempt.
    const missing = PORTABLE_PREF_KEYS.filter((key) => !mapped.has(key))
    expect(missing, `unmapped portable prefs: ${missing.join(', ')}`).toEqual([])
  })

  it('does not map keys that are not portable', () => {
    const portable = new Set<string>(PORTABLE_PREF_KEYS)
    const stray = [...portableConfigMappedKeys()].filter((key) => !portable.has(key))
    expect(stray, `mapped but not portable: ${stray.join(', ')}`).toEqual([])
  })
})
