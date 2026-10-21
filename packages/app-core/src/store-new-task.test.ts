// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from 'vitest'
import { TASKS_TAB_PATH } from '@shared/tasks'
import { makeLeaf } from './lib/pane-layout'

beforeEach(() => {
  vi.resetModules()
  localStorage.clear()
})

async function setup(onTasksView: boolean) {
  const createNote = vi.fn(async (_folder: string, title: string) => ({
    path: `inbox/${title}.md`,
    title,
    folder: 'inbox'
  }))
  const writeNote = vi.fn(async () => {})
  Object.defineProperty(window, 'zen', {
    configurable: true,
    value: { getCapabilities: () => ({}), createNote, writeNote }
  })
  const { useStore } = await import('./store')
  const { useToastStore } = await import('./lib/toast')
  const prompts = await import('./lib/prompt-requests')
  const selectNote = vi.fn(async () => {})
  const leaf = onTasksView ? makeLeaf([TASKS_TAB_PATH], TASKS_TAB_PATH) : makeLeaf(['inbox/Notes.md'], 'inbox/Notes.md')
  useStore.setState({
    vault: { root: '/test', name: 'Test' },
    paneLayout: leaf,
    activePaneId: leaf.id,
    refreshTasks: vi.fn(async () => {}),
    selectNote
  })
  const create = async (title: string | null): Promise<string | null> => {
    const pending = useStore.getState().newTaskFile()
    await vi.waitFor(() => expect(prompts.getPromptRequest()).not.toBeNull())
    prompts.settlePromptRequest(prompts.getPromptRequest()!, title)
    return pending
  }
  return { create, createNote, selectNote, toasts: () => useToastStore.getState().toasts }
}

describe('newTaskFile', () => {
  it('says the task landed, and opens it on request, when the Tasks view is not showing', async () => {
    const s = await setup(false)
    expect(await s.create('Buy milk')).toBe('inbox/Buy milk.md')
    const toast = s.toasts().find((t) => t.message === 'Task added')
    expect(toast?.type).toBe('success')
    expect(toast?.action?.label).toBe('Open')
    toast?.action?.onClick()
    expect(s.selectNote).toHaveBeenCalledWith('inbox/Buy milk.md')
  })

  it('stays quiet on the Tasks view, where the new task appears in the list', async () => {
    const s = await setup(true)
    expect(await s.create('Buy milk')).toBe('inbox/Buy milk.md')
    expect(s.toasts().some((t) => t.message === 'Task added')).toBe(false)
  })

  it('says nothing when the prompt is cancelled', async () => {
    const s = await setup(false)
    expect(await s.create(null)).toBeNull()
    expect(s.createNote).not.toHaveBeenCalled()
    expect(s.toasts()).toEqual([])
  })
})
