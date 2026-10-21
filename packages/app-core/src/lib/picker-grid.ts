import type { KeyboardEvent as ReactKeyboardEvent } from 'react'

/**
 * Keyboard movement for the sidebar's icon and color pickers (#960): a grid of
 * buttons that opens with one of them focused, so a key moves focus to a
 * neighbour and Enter presses it like any button. Arrows always move; h/j/k/l
 * move only with Vim mode on, since single-key motions are Vim-only
 * everywhere else. The grid reflows from three columns to two on a narrow
 * window, so the column count is read from the rendered rows, never assumed.
 */

export type PickerGridMove = 'left' | 'right' | 'up' | 'down'

export function pickerGridMoveForKey(key: string, vimMode: boolean): PickerGridMove | null {
  switch (key) {
    case 'ArrowLeft':
      return 'left'
    case 'ArrowRight':
      return 'right'
    case 'ArrowUp':
      return 'up'
    case 'ArrowDown':
      return 'down'
  }
  if (!vimMode) return null
  switch (key) {
    case 'h':
      return 'left'
    case 'l':
      return 'right'
    case 'k':
      return 'up'
    case 'j':
      return 'down'
  }
  return null
}

/** Left and right walk the cells in reading order, so they cross rows; up and
 *  down keep the column. A move past an edge, or down into the gap a short
 *  last row leaves, stays where it is. */
export function stepPickerGrid(
  index: number,
  count: number,
  columns: number,
  move: PickerGridMove
): number {
  if (count <= 0) return -1
  const cols = Math.max(1, columns)
  const next =
    move === 'left'
      ? index - 1
      : move === 'right'
        ? index + 1
        : move === 'up'
          ? index - cols
          : index + cols
  return next >= 0 && next < count ? next : index
}

/** Cells sharing the first cell's top edge form the first row. */
function renderedColumns(cells: HTMLElement[]): number {
  const top = cells[0]?.offsetTop
  let columns = 0
  for (const cell of cells) {
    if (cell.offsetTop !== top) break
    columns += 1
  }
  return columns
}

/** onKeyDown for the grid container; its cells carry `data-picker-option`. */
export function handlePickerGridKey(e: ReactKeyboardEvent<HTMLElement>, vimMode: boolean): void {
  if (e.metaKey || e.ctrlKey || e.altKey) return
  const move = pickerGridMoveForKey(e.key, vimMode)
  if (!move) return
  const cells = [...e.currentTarget.querySelectorAll<HTMLElement>('[data-picker-option]')]
  const index = cells.indexOf(document.activeElement as HTMLElement)
  if (index < 0) return
  e.preventDefault()
  e.stopPropagation()
  const next = stepPickerGrid(index, cells.length, renderedColumns(cells), move)
  cells[next]?.focus()
}
