import { describe, expect, it } from 'vitest'
import {
  groupTasks,
  inferDailyTaskDueDates,
  parseTaskFile,
  parseTasksFromBody,
  type ParseTasksContext
} from './tasks'

const ctx: ParseTasksContext = { path: 'inbox/t.md', title: 't', folder: 'inbox' }
const parse = (line: string) => parseTasksFromBody(line, ctx)[0]

describe('parseTasksFromBody — inline due dates (#343)', () => {
  it('parses a due date written without a space', () => {
    const task = parse('- [ ] pay rent due:2026-07-08')
    expect(task.due).toBe('2026-07-08')
    expect(task.content).not.toContain('due')
  })

  it('parses a due date written with a space (as the @-date completion inserts it)', () => {
    const task = parse('- [ ] pay rent due: 2026-07-08')
    expect(task.due).toBe('2026-07-08')
    expect(task.content).not.toContain('due')
    expect(task.content).not.toContain('2026')
  })

  it('leaves the due unset for a non-ISO candidate', () => {
    expect(parse('- [ ] task due:@tomorrow').due).toBeUndefined()
    expect(parse('- [ ] task due: someday').due).toBeUndefined()
  })
})

describe('groupTasks: a task with no due date has its own group (#990)', () => {
  const today = new Date(2026, 9, 23)
  const lines = (body: string) => parseTasksFromBody(body, ctx)

  it('keeps Today for what is due today or overdue', () => {
    const g = groupTasks(
      lines('- [ ] someday\n- [ ] now due:2026-10-23\n- [ ] late due:2026-10-20\n- [ ] next due:2026-11-04'),
      today
    )
    expect(g.today.map((t) => t.content)).toEqual(['late', 'now'])
    expect(g.overdueCount).toBe(1)
    expect(g.upcoming.map((t) => t.content)).toEqual(['next'])
    expect(g.undated.map((t) => t.content)).toEqual(['someday'])
  })

  it('puts a task note made with New task, which has only dateCreated, under no due date', () => {
    const file = parseTaskFile(
      '---\ntitle: Plan the trip\nstatus: open\ntags: [task]\ndateCreated: 2026-10-23T19:15:52.087Z\n---\n',
      { path: 'inbox/Plan the trip.md', title: 'Plan the trip', folder: 'inbox' }
    )
    expect(file).not.toBeNull()
    const g = groupTasks([file!], today)
    expect(g.undated.map((t) => t.content)).toEqual(['Plan the trip'])
    expect(g.today).toEqual([])
  })

  it('counts a daily-note task as due on its note\'s date once inferred', () => {
    const daily = parseTasksFromBody('- [ ] from the daily note', {
      path: 'daily/2026-10-23.md', title: '2026-10-23', folder: 'inbox'
    })
    const dated = inferDailyTaskDueDates(daily, new Map([['daily/2026-10-23.md', '2026-10-23']]))
    expect(groupTasks(dated, today).today.map((t) => t.content)).toEqual(['from the daily note'])
    expect(groupTasks(daily, today).undated.map((t) => t.content)).toEqual(['from the daily note'])
  })
})
