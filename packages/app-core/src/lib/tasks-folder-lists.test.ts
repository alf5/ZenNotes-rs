import { describe, expect, it } from 'vitest'
import { toggleTasksFolder } from './tasks-folder-lists'

describe('toggleTasksFolder (#458, #962)', () => {
  it('adds a folder to an empty list', () => {
    expect(toggleTasksFolder(undefined, 'includedFolders', 'Tasks')).toEqual({
      includedFolders: ['Tasks']
    })
  })

  it('removes a folder that is already listed', () => {
    expect(
      toggleTasksFolder({ includedFolders: ['Tasks', 'Work'] }, 'includedFolders', 'Tasks')
    ).toEqual({ includedFolders: ['Work'] })
  })

  it('keeps the other list when toggling one', () => {
    expect(
      toggleTasksFolder({ includedFolders: ['Projects'] }, 'excludedFolders', 'Projects/Archive')
    ).toEqual({ includedFolders: ['Projects'], excludedFolders: ['Projects/Archive'] })
    expect(
      toggleTasksFolder(
        { includedFolders: ['Projects'], excludedFolders: ['Projects/Archive'] },
        'includedFolders',
        'Projects'
      )
    ).toEqual({ excludedFolders: ['Projects/Archive'] })
  })

  it('leaves no empty stub behind', () => {
    expect(toggleTasksFolder({ excludedFolders: ['Books'] }, 'excludedFolders', 'Books')).toBeUndefined()
  })
})
