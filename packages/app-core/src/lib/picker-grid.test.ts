import { describe, expect, it } from 'vitest'
import { pickerGridMoveForKey, stepPickerGrid } from './picker-grid'

describe('pickerGridMoveForKey', () => {
  it('moves with the arrows whatever the Vim setting', () => {
    for (const vimMode of [true, false]) {
      expect(pickerGridMoveForKey('ArrowLeft', vimMode)).toBe('left')
      expect(pickerGridMoveForKey('ArrowRight', vimMode)).toBe('right')
      expect(pickerGridMoveForKey('ArrowUp', vimMode)).toBe('up')
      expect(pickerGridMoveForKey('ArrowDown', vimMode)).toBe('down')
    }
  })

  it('moves with h j k l only in Vim mode', () => {
    expect(pickerGridMoveForKey('h', true)).toBe('left')
    expect(pickerGridMoveForKey('j', true)).toBe('down')
    expect(pickerGridMoveForKey('k', true)).toBe('up')
    expect(pickerGridMoveForKey('l', true)).toBe('right')
    for (const key of ['h', 'j', 'k', 'l']) {
      expect(pickerGridMoveForKey(key, false)).toBeNull()
    }
  })

  it('ignores every other key', () => {
    expect(pickerGridMoveForKey('Enter', true)).toBeNull()
    expect(pickerGridMoveForKey('J', true)).toBeNull()
    expect(pickerGridMoveForKey('x', true)).toBeNull()
  })
})

describe('stepPickerGrid', () => {
  // The color picker: ten cells in three columns, a lone cell on the last row.
  const count = 10
  const columns = 3

  it('walks left and right in reading order, across rows', () => {
    expect(stepPickerGrid(2, count, columns, 'right')).toBe(3)
    expect(stepPickerGrid(3, count, columns, 'left')).toBe(2)
  })

  it('keeps the column moving up and down', () => {
    expect(stepPickerGrid(1, count, columns, 'down')).toBe(4)
    expect(stepPickerGrid(9, count, columns, 'up')).toBe(6)
  })

  it('stays put at every edge', () => {
    expect(stepPickerGrid(0, count, columns, 'left')).toBe(0)
    expect(stepPickerGrid(0, count, columns, 'up')).toBe(0)
    expect(stepPickerGrid(9, count, columns, 'right')).toBe(9)
    expect(stepPickerGrid(9, count, columns, 'down')).toBe(9)
  })

  it('does not drop into the gap a short last row leaves', () => {
    expect(stepPickerGrid(7, count, columns, 'down')).toBe(7)
    expect(stepPickerGrid(6, count, columns, 'down')).toBe(9)
  })

  it('follows the two-column layout of a narrow window', () => {
    expect(stepPickerGrid(1, count, 2, 'down')).toBe(3)
  })

  it('treats a missing column count as a single column', () => {
    expect(stepPickerGrid(1, count, 0, 'down')).toBe(2)
  })
})
