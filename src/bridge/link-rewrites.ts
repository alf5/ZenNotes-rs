/**
 * Links that follow a rename — the bridge half of upstream's renameNote
 * (`updateInboundWikilinks`, #111) and renameAsset / moveAsset
 * (`updateAssetReferences`, #785) in apps/desktop/src/main/vault.ts.
 *
 * Upstream does this in its main process after the file moves; here it runs
 * in the webview over the vendored shared-domain rewriters, the same split as
 * the CSV databases: Rust moves the file, this decides what every other note
 * should now say. The vault is snapshotted BEFORE the move so links still
 * resolve to the old name, and only notes that can hold such a link are read.
 * A note that fails to rewrite is logged and skipped, as upstream does: the
 * rename itself has already happened and must not be reported as failed.
 */

import { invoke } from '@tauri-apps/api/core'
import type { AssetMeta, NoteContent, NoteMeta } from '@zennotes/bridge-contract/ipc'
import {
  resolveWikilinkTarget,
  rewriteWikilinksForRename,
  type RenameNoteRef
} from '@zennotes/shared-domain/wikilink-rename'
import { rewriteAssetReferences } from '@zennotes/shared-domain/asset-link-rename'
import { resolveAssetPathAmong } from '@zennotes/shared-domain/asset-path-resolution'

const listNotes = (): Promise<NoteMeta[]> => invoke('vault_list_notes')
const listAssets = (): Promise<AssetMeta[]> => invoke('vault_list_assets')
const readNote = (relPath: string): Promise<NoteContent> => invoke('vault_read_note', { relPath })
const writeNote = (relPath: string, body: string): Promise<NoteMeta> =>
  invoke('vault_write_note', { relPath, body })

/** Rename a note, then point every `[[wikilink]]` that named it at the new
 *  title. */
export async function renameNoteWithLinks(relPath: string, nextTitle: string): Promise<NoteMeta> {
  const notesBefore = await listNotes()
  const meta = await invoke<NoteMeta>('vault_rename_note', { relPath, nextTitle })
  if (meta.path !== relPath) await updateInboundWikilinks(notesBefore, relPath, meta.title)
  return meta
}

async function updateInboundWikilinks(
  notesBefore: NoteMeta[],
  oldPath: string,
  newTitle: string
): Promise<void> {
  const refs: RenameNoteRef[] = notesBefore.map((n) => ({
    path: n.path,
    title: n.title,
    folder: n.folder
  }))
  const candidates = notesBefore.filter(
    (n) =>
      n.path !== oldPath &&
      n.folder !== 'trash' &&
      (n.wikilinks ?? []).some(
        (t) => resolveWikilinkTarget(refs, t.split(/[|#^]/, 1)[0])?.path === oldPath
      )
  )
  for (const candidate of candidates) {
    try {
      const content = await readNote(candidate.path)
      const { body, changed } = rewriteWikilinksForRename(content.body, refs, oldPath, newTitle)
      if (changed > 0) await writeNote(candidate.path, body)
    } catch (err) {
      console.error('updateInboundWikilinks: failed for', candidate.path, err)
    }
  }
}

/** Upstream's `localAssetTargetKind(t) !== null`: a link target naming a
 *  file with an extension, which is what an asset reference can be. */
function namesFile(target: string): boolean {
  const clean = target.split('#')[0]?.split('?')[0] ?? target
  return clean.lastIndexOf('.') !== -1
}

/** Rename or move an asset, then rewrite every `![[embed]]`, `[[link]]`,
 *  `![](href)` and `[](href)` that resolved to it. */
export async function relocateAssetWithReferences(
  relPath: string,
  move: () => Promise<AssetMeta>
): Promise<AssetMeta> {
  const [assetsBefore, notesBefore] = await Promise.all([listAssets(), listNotes()])
  const meta = await move()
  if (meta.path === relPath) return meta
  const candidates = notesBefore.filter(
    (n) =>
      n.folder !== 'trash' &&
      (n.hasAttachments ||
        (n.assetEmbeds ?? []).length > 0 ||
        (n.wikilinks ?? []).some(
          (t) => namesFile(t) && resolveAssetPathAmong(assetsBefore, n.path, t) === relPath
        ))
  )
  for (const candidate of candidates) {
    try {
      const content = await readNote(candidate.path)
      const { body, changed } = rewriteAssetReferences(
        content.body,
        assetsBefore,
        candidate.path,
        relPath,
        meta.path
      )
      if (changed > 0) await writeNote(candidate.path, body)
    } catch (err) {
      console.error('updateAssetReferences: failed for', candidate.path, err)
    }
  }
  return meta
}
