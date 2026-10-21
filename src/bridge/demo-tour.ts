/**
 * The demo tour — port of `generateDemoTour` / `removeDemoTour` in upstream's
 * apps/desktop/src/main/vault.ts.
 *
 * The tour's notes and assets are vendored shared-domain data
 * (`@shared/demo-tour-data`), so this file only places them; a re-vendor
 * brings a rewritten tour with it. (It used to be a JSON copy embedded in the
 * Rust binary, which had stayed at the v2.1 tour.) The bridge imports this
 * module on demand, keeping the ~60 KB of tour text off the boot path.
 */

import { invoke } from '@tauri-apps/api/core'
import type { VaultDemoTourResult } from '@zennotes/bridge-contract/ipc'
import { DEMO_TOUR_ASSETS, DEMO_TOUR_NOTES } from '@zennotes/shared-domain/demo-tour-data'
import {
  currentDemoTourPath,
  DEMO_TOUR_DIR,
  demoTourNestedDirs
} from '@zennotes/shared-domain/demo-tour'

const writeText = (relPath: string, text: string): Promise<void> =>
  invoke('db_write_text', { relPath, text })
const removeFile = (relPath: string): Promise<void> =>
  invoke<void>('db_delete_file', { relPath }).catch(() => undefined)
const removeDirIfEmpty = (relPath: string): Promise<void> =>
  invoke<void>('db_remove_dir_if_empty', { relPath }).catch(() => undefined)

/** Tour notes still on disk under their older names (see currentDemoTourPath). */
async function olderDemoTourNotePaths(): Promise<string[]> {
  const current = new Set(DEMO_TOUR_NOTES.map((note) => note.path))
  let names: string[]
  try {
    names = await invoke<string[]>('db_list_dir', { relPath: DEMO_TOUR_DIR })
  } catch {
    return []
  }
  return names
    .map((name) => `${DEMO_TOUR_DIR}/${name}`)
    .filter((rel) => rel !== currentDemoTourPath(rel) && current.has(currentDemoTourPath(rel)))
}

export async function generateDemoTour(): Promise<VaultDemoTourResult> {
  await invoke('db_ensure_vault_layout')
  // Regenerating replaces a tour seeded under the older names rather than
  // writing a second copy beside it.
  for (const rel of await olderDemoTourNotePaths()) await removeFile(rel)
  for (const note of DEMO_TOUR_NOTES) await writeText(note.path, note.body)
  for (const asset of DEMO_TOUR_ASSETS) await writeText(asset.path, asset.body)
  return {
    notePaths: DEMO_TOUR_NOTES.map((note) => note.path),
    assetPaths: DEMO_TOUR_ASSETS.map((asset) => asset.path)
  }
}

export async function removeDemoTour(): Promise<VaultDemoTourResult> {
  const notePaths = [...DEMO_TOUR_NOTES.map((note) => note.path), ...(await olderDemoTourNotePaths())]
  for (const rel of notePaths) await removeFile(rel)
  for (const asset of DEMO_TOUR_ASSETS) await removeFile(asset.path)
  for (const dir of demoTourNestedDirs(DEMO_TOUR_ASSETS.map((asset) => asset.path))) {
    await removeDirIfEmpty(dir)
  }
  await removeDirIfEmpty(DEMO_TOUR_DIR)
  return { notePaths, assetPaths: DEMO_TOUR_ASSETS.map((asset) => asset.path) }
}
