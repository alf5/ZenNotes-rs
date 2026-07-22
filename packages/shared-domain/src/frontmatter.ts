// Minimal frontmatter editing for whole-note "file tasks" (TaskNotes-style).
// Adds/updates/removes flat `key: value` scalars in a leading `---` block,
// creating the block if absent, and leaves every other line — including block
// lists like `tags:\n  - task` — byte-identical. This is deliberately not a
// full YAML writer; it only touches the exact keys it is asked to.
import type { TaskPriority } from './tasks'

function yamlValue(value: string): string {
  // Quote when the value could be misread as YAML structure or has edge
  // whitespace; JSON.stringify yields a valid double-quoted, escaped scalar.
  if (/[:#"'\n]/.test(value) || value.trim() !== value || value === '') {
    return JSON.stringify(value)
  }
  return value
}

/**
 * Set (or, with a `null` value, remove) scalar frontmatter fields in `body`,
 * preserving key order and every other line. Creates a frontmatter block from
 * the non-null updates when the note has none. Keys match case-insensitively
 * but are written with the casing given in `updates`.
 */
export function updateFrontmatterFields(
  body: string,
  updates: Record<string, string | null>
): string {
  const entries = Object.entries(updates)
  if (entries.length === 0) return body

  const norm = body.replace(/\r\n/g, '\n')
  const m = norm.match(/^---\n([\s\S]*?)\n---\n?/)
  const remaining = new Map(entries.map(([k, v]) => [k.toLowerCase(), { key: k, value: v }]))

  if (!m) {
    const lines = ['---']
    for (const { key, value } of remaining.values()) {
      if (value != null) lines.push(`${key}: ${yamlValue(value)}`)
    }
    if (lines.length === 1) return body // nothing to add
    lines.push('---', '')
    return lines.join('\n') + norm
  }

  const out: string[] = []
  for (const line of m[1].split('\n')) {
    const km = line.match(/^\s*([A-Za-z0-9_][\w-]*)\s*:/)
    if (km) {
      const lk = km[1].toLowerCase()
      const upd = remaining.get(lk)
      if (upd) {
        remaining.delete(lk)
        if (upd.value != null) out.push(`${upd.key}: ${yamlValue(upd.value)}`)
        continue // a null value drops the line
      }
    }
    out.push(line)
  }
  for (const { key, value } of remaining.values()) {
    if (value != null) out.push(`${key}: ${yamlValue(value)}`)
  }
  return `---\n${out.join('\n')}\n---\n` + norm.slice(m[0].length)
}

/** Flip a file-task's completion in its frontmatter: `status: done` +
 *  `completedDate` when checking, back to `open` (clearing `completedDate`)
 *  when unchecking. `todayIso` is the local YYYY-MM-DD to stamp. */
export function setTaskFileStatus(body: string, done: boolean, todayIso: string): string {
  return updateFrontmatterFields(body, {
    status: done ? 'done' : 'open',
    completedDate: done ? todayIso : null
  })
}

/** ZenNotes priority -> the value written to a task file's frontmatter, using
 *  TaskNotes' vocabulary (`high` / `normal` / `low`) for interop. `null` clears. */
export function taskFilePriorityValue(priority: TaskPriority | null | undefined): string | null {
  if (priority === 'high') return 'high'
  if (priority === 'med') return 'normal'
  if (priority === 'low') return 'low'
  return null
}

export interface ComposeTaskFileInput {
  title: string
  /** Defaults to `open`. */
  status?: string
  priority?: TaskPriority
  /** ISO YYYY-MM-DD. */
  due?: string
  /** ISO YYYY-MM-DD. */
  scheduled?: string
  /** Extra tags beyond the mandatory `task` tag. */
  tags?: string[]
  /** ISO timestamp for `dateCreated` (caller supplies the clock). */
  dateCreated?: string
  /** Free-form note body after the frontmatter. */
  body?: string
}

/** Build a new task-file `.md` (frontmatter + body) in the TaskNotes shape. The
 *  `task` tag is always present so the note is recognized as a task. */
export function composeTaskFile(input: ComposeTaskFileInput): string {
  const tags = [TASK_TAG, ...(input.tags ?? []).filter((t) => t && t !== TASK_TAG)]
  const lines = ['---', `title: ${yamlValue(input.title)}`, `status: ${input.status ?? 'open'}`]
  const priority = taskFilePriorityValue(input.priority)
  if (priority) lines.push(`priority: ${priority}`)
  if (input.due) lines.push(`due: ${input.due}`)
  if (input.scheduled) lines.push(`scheduled: ${input.scheduled}`)
  lines.push(`tags: [${tags.join(', ')}]`)
  if (input.dateCreated) lines.push(`dateCreated: ${input.dateCreated}`)
  lines.push('---', '')
  return `${lines.join('\n')}\n${(input.body ?? '').replace(/^\n+/, '')}`
}

// Kept local to avoid a cyclic import with tasks.ts; must equal TASK_FILE_TAG.
const TASK_TAG = 'task'
