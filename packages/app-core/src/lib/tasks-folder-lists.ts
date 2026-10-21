import type { VaultSettings } from '@shared/ipc'

/** The two folder lists under a vault's `tasks` settings: the folders kept
 *  out of Tasks (#458), and the Task folders it is narrowed to (#962). */
export type TasksFolderList = 'excludedFolders' | 'includedFolders'

/**
 * The vault's `tasks` settings with `relDir` added to or removed from one of
 * its folder lists, the other list kept as it is. The exclusion toggle used to
 * rebuild `tasks` from its own list alone, which would have dropped the Task
 * folders the moment a folder was excluded. Undefined when both lists end up
 * empty, so vault.json carries no empty stub.
 */
export function toggleTasksFolder(
  tasks: VaultSettings['tasks'],
  list: TasksFolderList,
  relDir: string
): VaultSettings['tasks'] {
  const current = tasks?.[list] ?? []
  const nextList = current.includes(relDir)
    ? current.filter((folder) => folder !== relDir)
    : [...current, relDir]
  const next: NonNullable<VaultSettings['tasks']> = { ...tasks, [list]: nextList }
  if (!next.excludedFolders?.length) delete next.excludedFolders
  if (!next.includedFolders?.length) delete next.includedFolders
  return Object.keys(next).length > 0 ? next : undefined
}
