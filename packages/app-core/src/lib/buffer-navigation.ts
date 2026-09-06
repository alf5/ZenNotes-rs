import type { NoteMeta } from '@shared/ipc'
import { allLeaves, findLeaf, type PaneLayout } from './pane-layout'

export type BufferNavigationTarget =
  | { kind: 'focus'; paneId: string; path: string }
  | { kind: 'open'; paneId: string; path: string }
  | { kind: 'create-quick' }
  | { kind: 'none' }

type BufferNote = Pick<NoteMeta, 'path' | 'folder' | 'updatedAt'>

export interface BufferNavigationRuntime {
  paneLayout: PaneLayout
  activePaneId: string
  notes: BufferNote[]
  focusTabInPane: (paneId: string, path: string) => Promise<void>
  openNoteInPane: (paneId: string, path: string) => Promise<void>
  createAndOpen: (
    folder: 'quick',
    subpath: string,
    options: { focusTitle: boolean }
  ) => Promise<unknown>
}

/** The order gt/gT cycle through and {count}gt / Alt+digits index into: every
 *  pane's tabs, deduped, in pane-tree order. One list so cycling and direct
 *  selection can never disagree about which tab is "number 3". */
function openTabOrder(paneLayout: PaneLayout): string[] {
  const seen = new Set<string>()
  const order: string[] = []
  for (const candidate of allLeaves(paneLayout)) {
    for (const path of candidate.tabs) {
      if (seen.has(path)) continue
      seen.add(path)
      order.push(path)
    }
  }
  return order
}

function targetFor(
  paneLayout: PaneLayout,
  leafId: string,
  leafTabs: string[],
  path: string
): BufferNavigationTarget {
  const owningLeaf = allLeaves(paneLayout).find((candidate) =>
    candidate.tabs.includes(path)
  )
  if (owningLeaf && owningLeaf.id !== leafId) {
    return { kind: 'focus', paneId: owningLeaf.id, path }
  }
  if (leafTabs.includes(path)) {
    return { kind: 'focus', paneId: leafId, path }
  }
  return { kind: 'open', paneId: leafId, path }
}

export function getBufferNavigationTarget(
  paneLayout: PaneLayout,
  activePaneId: string,
  notes: BufferNote[],
  delta: number
): BufferNavigationTarget {
  const leaf = findLeaf(paneLayout, activePaneId)
  if (!leaf) return { kind: 'none' }

  const order = openTabOrder(paneLayout)
  const seen = new Set(order)

  if (order.length < 2) {
    const fallback = notes
      .filter((note) => note.folder !== 'trash')
      .slice()
      .sort((a, b) => b.updatedAt - a.updatedAt)
    for (const note of fallback) {
      if (seen.has(note.path)) continue
      seen.add(note.path)
      order.push(note.path)
    }
  }

  if (order.length < 2) return { kind: 'create-quick' }

  const baseIndex = leaf.activeTab ? order.indexOf(leaf.activeTab) : -1
  const startIndex = baseIndex >= 0 ? baseIndex : 0
  // Proper modulo: {count}gT walks back count tabs, which can pass -length.
  const nextIndex = (((startIndex + delta) % order.length) + order.length) % order.length
  return targetFor(paneLayout, leaf.id, leaf.tabs, order[nextIndex])
}

/** Direct selection for {count}gt and the Alt+digit shortcuts: 1-based index
 *  into the open-tab order. An index past the end lands on the last tab, the
 *  same forgiving read vim gives a too-large {count}gt. Never falls back to
 *  recent notes: "tab 3" means an open tab or nothing. */
export function getBufferSelectTarget(
  paneLayout: PaneLayout,
  activePaneId: string,
  index: number
): BufferNavigationTarget {
  const leaf = findLeaf(paneLayout, activePaneId)
  if (!leaf) return { kind: 'none' }

  const order = openTabOrder(paneLayout)
  if (order.length === 0) return { kind: 'none' }

  const clamped = Math.min(Math.max(Math.trunc(index), 1), order.length)
  return targetFor(paneLayout, leaf.id, leaf.tabs, order[clamped - 1])
}

function applyTarget(runtime: BufferNavigationRuntime, target: BufferNavigationTarget): void {
  if (target.kind === 'focus') {
    void runtime.focusTabInPane(target.paneId, target.path)
    return
  }
  if (target.kind === 'open') {
    void runtime.openNoteInPane(target.paneId, target.path)
    return
  }
  if (target.kind === 'create-quick') {
    void runtime.createAndOpen('quick', '', { focusTitle: true })
  }
}

export function navigateActiveBuffer(
  runtime: BufferNavigationRuntime,
  delta: number
): void {
  applyTarget(
    runtime,
    getBufferNavigationTarget(runtime.paneLayout, runtime.activePaneId, runtime.notes, delta)
  )
}

export function selectActiveBuffer(runtime: BufferNavigationRuntime, index: number): void {
  applyTarget(
    runtime,
    getBufferSelectTarget(runtime.paneLayout, runtime.activePaneId, index)
  )
}
