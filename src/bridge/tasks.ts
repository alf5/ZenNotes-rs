/**
 * Tasks — port of apps/desktop/src/main/tasks.ts over the Rust file layer.
 *
 * Parsing runs here through vendored `@shared/tasks`, exactly the functions
 * upstream's main process calls, so the Tasks view reads every task the way
 * upstream does — `[>]` forwarded, `[-]` cancelled, `[/]` in progress,
 * `@field:value` fields, whole-note `#task` files — and the next re-vendor
 * carries any new rule with it. (This used to be a Rust port of the v2.1
 * parser, which knew none of those.) Rust only says which notes are live and
 * hands over their text, in one round trip each.
 */

import { invoke } from './ipc'
import type { NoteFolder, VaultSettings } from '@zennotes/bridge-contract/ipc'
import { parseTaskFile, parseTasksFromBody, type VaultTask } from '@zennotes/shared-domain/tasks'
import { isPathExcludedFromTasks } from '@zennotes/shared-domain/tasks-excluded-folders'

interface TaskSource {
  path: string
  title: string
  folder: NoteFolder
}

/** Emit a note's file-task (if its frontmatter tags it `#task`) plus every
 *  inline `- [ ]` checkbox in its body. The file-task comes first so it heads
 *  the note's group in the Tasks list. */
function parseAllTasks(body: string, ctx: TaskSource): VaultTask[] {
  const fileTask = parseTaskFile(body, ctx)
  const inline = parseTasksFromBody(body, ctx)
  return fileTask ? [fileTask, ...inline] : inline
}

async function taskFolderLists(): Promise<{ excluded: string[]; included: string[] }> {
  const settings = await invoke<VaultSettings>('vault_get_settings')
  return {
    excluded: settings.tasks?.excludedFolders ?? [],
    included: settings.tasks?.includedFolders ?? []
  }
}

/** Every task in every live (non-trash) note. Folders on the vault's
 *  `tasks.excludedFolders` list (#458), and every folder outside its Task
 *  folders once `tasks.includedFolders` names any (#962), are skipped before
 *  any file is read. */
export async function scanTasks(): Promise<VaultTask[]> {
  const [{ excluded, included }, sources] = await Promise.all([
    taskFolderLists(),
    invoke<TaskSource[]>('vault_task_sources')
  ])
  const wanted = sources.filter((s) => !isPathExcludedFromTasks(s.path, excluded, included))
  const bodies = await invoke<(string | null)[]>('db_read_texts', {
    relPaths: wanted.map((s) => s.path)
  })
  const out: VaultTask[] = []
  wanted.forEach((source, i) => {
    const body = bodies[i]
    if (body != null) out.push(...parseAllTasks(body, source))
  })
  return out
}

/** Rescan a single note. Empty when the note is gone, outside a live folder
 *  or left out by the folder lists — the caller still uses the result to
 *  drop stale rows, so a single-note rescan must apply the same lists as the
 *  full scan or it would resurrect a left-out folder's tasks on every edit. */
export async function scanTasksForPath(relPath: string): Promise<VaultTask[]> {
  const source = await invoke<TaskSource | null>('vault_task_source_for', { relPath })
  if (!source) return []
  const { excluded, included } = await taskFolderLists()
  if (isPathExcludedFromTasks(source.path, excluded, included)) return []
  const body = await invoke<string | null>('db_read_text', { relPath: source.path })
  return body == null ? [] : parseAllTasks(body, source)
}
