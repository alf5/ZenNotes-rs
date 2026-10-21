import { describe, expect, it } from 'vitest'
import {
  isPathExcludedFromTasks,
  normalizeTasksExcludedFolder,
  normalizeTasksExcludedFolders,
  normalizeTasksIncludedFolders,
  rewriteTasksFoldersForRename
} from './tasks-excluded-folders'

describe('normalizeTasksExcludedFolder (#458)', () => {
  it('cleans slashes and whitespace', () => {
    expect(normalizeTasksExcludedFolder('inbox/Books')).toBe('inbox/Books')
    expect(normalizeTasksExcludedFolder('/inbox/Books/')).toBe('inbox/Books')
    expect(normalizeTasksExcludedFolder('inbox//Books')).toBe('inbox/Books')
    expect(normalizeTasksExcludedFolder('inbox\\Books')).toBe('inbox/Books')
    expect(normalizeTasksExcludedFolder('  inbox / Books ')).toBe('inbox/Books')
  })

  it('rejects traversal, dot segments, empties, and non-strings', () => {
    expect(normalizeTasksExcludedFolder('../outside')).toBeNull()
    expect(normalizeTasksExcludedFolder('inbox/../trash')).toBeNull()
    expect(normalizeTasksExcludedFolder('./inbox')).toBeNull()
    expect(normalizeTasksExcludedFolder('')).toBeNull()
    expect(normalizeTasksExcludedFolder('   ')).toBeNull()
    expect(normalizeTasksExcludedFolder(42)).toBeNull()
    expect(normalizeTasksExcludedFolder(null)).toBeNull()
  })
})

describe('normalizeTasksExcludedFolders (#458)', () => {
  it('drops invalid entries and duplicates, preserving order', () => {
    expect(
      normalizeTasksExcludedFolders(['inbox/Books', '../x', 'inbox/Books/', 'archive/Old'])
    ).toEqual(['inbox/Books', 'archive/Old'])
  })

  it('yields [] for malformed vault.json values', () => {
    expect(normalizeTasksExcludedFolders(undefined)).toEqual([])
    expect(normalizeTasksExcludedFolders('inbox/Books')).toEqual([])
    expect(normalizeTasksExcludedFolders({ excludedFolders: [] })).toEqual([])
  })
})

describe('isPathExcludedFromTasks (#458)', () => {
  const excluded = ['inbox/Books', 'archive/Old Projects']

  it('matches notes inside an excluded folder, any depth', () => {
    expect(isPathExcludedFromTasks('inbox/Books/dune.md', excluded)).toBe(true)
    expect(isPathExcludedFromTasks('inbox/Books/scifi/blindsight.md', excluded)).toBe(true)
    expect(isPathExcludedFromTasks('archive/Old Projects/site.md', excluded)).toBe(true)
  })

  it('is a segment match, not a substring match', () => {
    expect(isPathExcludedFromTasks('inbox/Bookshelf.md', excluded)).toBe(false)
    expect(isPathExcludedFromTasks('inbox/Books.md', excluded)).toBe(false)
  })

  it('is case-sensitive like the rest of the vault layer', () => {
    expect(isPathExcludedFromTasks('inbox/books/dune.md', excluded)).toBe(false)
  })

  it('never matches with an empty list', () => {
    expect(isPathExcludedFromTasks('inbox/Books/dune.md', [])).toBe(false)
  })
})

describe('isPathExcludedFromTasks with Task folders (#962)', () => {
  it('keeps the whole vault when no Task folders are set', () => {
    expect(isPathExcludedFromTasks('inbox/Idea.md', [], [])).toBe(false)
    expect(isPathExcludedFromTasks('inbox/Idea.md', [])).toBe(false)
  })

  it('leaves out every note outside the Task folders', () => {
    expect(isPathExcludedFromTasks('Tasks/Today.md', [], ['Tasks'])).toBe(false)
    expect(isPathExcludedFromTasks('Tasks/Work/Q4.md', [], ['Tasks'])).toBe(false)
    expect(isPathExcludedFromTasks('inbox/Books.md', [], ['Tasks'])).toBe(true)
    expect(isPathExcludedFromTasks('TasksArchive/Old.md', [], ['Tasks'])).toBe(true)
  })

  it('counts a note in any of several Task folders', () => {
    const included = ['Tasks', 'inbox/Projects']
    expect(isPathExcludedFromTasks('inbox/Projects/Site.md', [], included)).toBe(false)
    expect(isPathExcludedFromTasks('inbox/Reading.md', [], included)).toBe(true)
  })

  it('still excludes a folder inside a Task folder', () => {
    expect(
      isPathExcludedFromTasks('Projects/Archive/Old.md', ['Projects/Archive'], ['Projects'])
    ).toBe(true)
    expect(
      isPathExcludedFromTasks('Projects/Live.md', ['Projects/Archive'], ['Projects'])
    ).toBe(false)
  })

  it('validates Task folders like excluded ones', () => {
    expect(normalizeTasksIncludedFolders(['Tasks/', '../x', 'Tasks', 'inbox\\Work'])).toEqual([
      'Tasks',
      'inbox/Work'
    ])
  })
})

describe('rewriteTasksFoldersForRename (#962)', () => {
  it('moves a renamed folder and the folders inside it', () => {
    expect(
      rewriteTasksFoldersForRename(
        { includedFolders: ['inbox/Tasks', 'inbox/Work'], excludedFolders: ['inbox/Tasks/Someday'] },
        'inbox/Tasks',
        'inbox/Todo'
      )
    ).toEqual({ includedFolders: ['inbox/Todo', 'inbox/Work'], excludedFolders: ['inbox/Todo/Someday'] })
  })

  it('follows a move into another folder', () => {
    expect(
      rewriteTasksFoldersForRename({ includedFolders: ['inbox/Tasks'] }, 'inbox/Tasks', 'inbox/Areas/Tasks')
    ).toEqual({ includedFolders: ['inbox/Areas/Tasks'] })
  })

  it('leaves look-alike names and other folders alone', () => {
    const tasks = { includedFolders: ['inbox/TasksArchive', 'quick/Tasks'] }
    expect(rewriteTasksFoldersForRename(tasks, 'inbox/Tasks', 'inbox/Todo')).toEqual(tasks)
  })

  it('collapses a rename onto an entry that is already listed', () => {
    expect(
      rewriteTasksFoldersForRename({ includedFolders: ['inbox/A', 'inbox/B'] }, 'inbox/A', 'inbox/B')
    ).toEqual({ includedFolders: ['inbox/B'] })
  })

  it('passes absent settings through', () => {
    expect(rewriteTasksFoldersForRename(undefined, 'inbox/A', 'inbox/B')).toBeUndefined()
  })
})
