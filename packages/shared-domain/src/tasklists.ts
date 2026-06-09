// Shared markdown task-list primitives used by both the renderer (toggling
// checkboxes in the preview) and the main-process vault-wide task scanner.
// The index convention here MUST stay in lockstep with any parser that wants
// to round-trip a toggle — see `src/shared/tasks.ts`.

export const FENCE_RE = /^(\s*)(```|~~~)/
export const TASK_LINE_RE = /^(\s*(?:>\s*)*(?:[-+*]|\d+[.)])\s+\[)( |x|X)(\].*)$/

export type TaskPriority = 'high' | 'med' | 'low'

/** Internal: walk to the task line at `taskIndex`, hand its
 *  TASK_LINE_RE match to `mutate`, and splice the result back in.
 *  Returns the markdown unchanged if `mutate` returns null/the same
 *  line, or the index is out of range. */
function editTaskAtIndex(
  markdown: string,
  taskIndex: number,
  mutate: (match: RegExpMatchArray) => string | null
): string {
  if (taskIndex < 0) return markdown

  const lines = markdown.split('\n')
  let currentTaskIndex = 0
  let inFence = false
  let fenceMarker: string | null = null

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    const fenceMatch = line.match(FENCE_RE)
    if (fenceMatch) {
      const marker = fenceMatch[2]
      if (!inFence) {
        inFence = true
        fenceMarker = marker
      } else if (marker === fenceMarker) {
        inFence = false
        fenceMarker = null
      }
      continue
    }
    if (inFence) continue

    const taskMatch = line.match(TASK_LINE_RE)
    if (!taskMatch) continue
    if (currentTaskIndex !== taskIndex) {
      currentTaskIndex += 1
      continue
    }

    const next = mutate(taskMatch)
    if (next == null || next === line) return markdown
    lines[i] = next
    return lines.join('\n')
  }
  return markdown
}

export function toggleTaskAtIndex(
  markdown: string,
  taskIndex: number,
  checked: boolean
): string {
  return editTaskAtIndex(markdown, taskIndex, (match) => {
    return `${match[1]}${checked ? 'x' : ' '}${match[3]}`
  })
}

/** Same as `toggleTaskAtIndex`, with a name that matches the rest of
 *  the `setTask*` mutators. Kept as a separate export for callers that
 *  want the explicit naming. */
export function setTaskCheckedAtIndex(
  markdown: string,
  taskIndex: number,
  checked: boolean
): string {
  return toggleTaskAtIndex(markdown, taskIndex, checked)
}

const WAITING_TOKEN_RE = /(^|\s)@waiting\b/i

/** Add or remove the `@waiting` marker on the task line at
 *  `taskIndex`. Adding inserts at the end of the tail with a single
 *  separating space; removing collapses any extra whitespace it left
 *  behind so the line stays tidy. */
export function setTaskWaitingAtIndex(
  markdown: string,
  taskIndex: number,
  waiting: boolean
): string {
  return editTaskAtIndex(markdown, taskIndex, (match) => {
    const prefix = match[1]
    const checkChar = match[2]
    const tailWithBracket = match[3]
    if (!tailWithBracket.startsWith(']')) return null
    const tail = tailWithBracket.slice(1)
    const has = WAITING_TOKEN_RE.test(tail)
    let nextTail = tail
    if (waiting && !has) {
      nextTail = `${tail.replace(/\s+$/u, '')} @waiting`
    } else if (!waiting && has) {
      nextTail = tail
        .replace(WAITING_TOKEN_RE, '$1')
        .replace(/\s{2,}/g, ' ')
        .replace(/\s+$/u, '')
    }
    return `${prefix}${checkChar}]${nextTail}`
  })
}

const PRIORITY_TOKEN_RE = /(^|\s)!(?:high|med|medium|low|h|m|l)\b/i
const DUE_TOKEN_RE = /(^|\s)due:\S+/i

/** Replace, insert, or remove the priority token (`!high|!med|!low`)
 *  on the task line at `taskIndex`. Pass `null` to clear. */
export function setTaskPriorityAtIndex(
  markdown: string,
  taskIndex: number,
  priority: TaskPriority | null
): string {
  return editTaskAtIndex(markdown, taskIndex, (match) => {
    const prefix = match[1]
    const checkChar = match[2]
    const tailWithBracket = match[3]
    if (!tailWithBracket.startsWith(']')) return null
    const tail = tailWithBracket.slice(1)
    const cleaned = tail.replace(PRIORITY_TOKEN_RE, '$1').replace(/\s{2,}/g, ' ')
    let nextTail: string
    if (priority) {
      // Append at the end so the inline content reads naturally before
      // the metadata token. Trim trailing whitespace so we don't
      // accumulate spaces across repeated mutations.
      nextTail = `${cleaned.replace(/\s+$/u, '')} !${priority}`
    } else {
      nextTail = cleaned.replace(/\s+$/u, '')
    }
    return `${prefix}${checkChar}]${nextTail}`
  })
}

/** Replace, insert, or remove the `due:YYYY-MM-DD` token on the task
 *  line at `taskIndex`. Pass `null` to clear. */
export function setTaskDueAtIndex(
  markdown: string,
  taskIndex: number,
  due: string | null
): string {
  return editTaskAtIndex(markdown, taskIndex, (match) => {
    const prefix = match[1]
    const checkChar = match[2]
    const tailWithBracket = match[3]
    if (!tailWithBracket.startsWith(']')) return null
    const tail = tailWithBracket.slice(1)
    const cleaned = tail.replace(DUE_TOKEN_RE, '$1').replace(/\s{2,}/g, ' ')
    let nextTail: string
    if (due) {
      nextTail = `${cleaned.replace(/\s+$/u, '')} due:${due}`
    } else {
      nextTail = cleaned.replace(/\s+$/u, '')
    }
    return `${prefix}${checkChar}]${nextTail}`
  })
}
