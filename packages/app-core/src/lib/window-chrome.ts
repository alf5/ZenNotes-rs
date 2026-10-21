import type { PaneLayout } from './pane-layout'

/**
 * Whether the workspace chrome has to carry the window drag itself: the user
 * turned the title bar off in a desktop window, so nothing else moves it (the
 * traffic lights go with the title bar on macOS). In a browser the page never
 * moves its window, so the answer there is always no.
 */
export function chromeCarriesWindowDrag(showWindowTitleBar: boolean, runtime: string): boolean {
  return runtime === 'desktop' && showWindowTitleBar === false
}

/**
 * Whether pane `paneId` touches the window's top edge: every column split
 * above it holds it in its first slot. Only those panes' tab strips stand in
 * for the title bar, as the top tab headers do in Obsidian; a tab strip in the
 * middle of the window is not a title bar.
 */
export function isPaneOnTopEdge(root: PaneLayout, paneId: string): boolean {
  if (root.kind === 'leaf') return root.id === paneId
  const children = root.direction === 'column' ? root.children.slice(0, 1) : root.children
  return children.some((child) => isPaneOnTopEdge(child, paneId))
}
