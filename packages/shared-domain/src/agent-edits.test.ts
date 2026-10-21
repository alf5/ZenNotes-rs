import { describe, expect, it } from 'vitest'
import {
  MAX_AGENT_EDIT_ENTRIES,
  MAX_AGENT_EDIT_NOTE_BYTES,
  MAX_AGENT_EDIT_OUTSIDE,
  agentEditBaseline,
  agentEditHasChanges,
  agentEditOrigin,
  agentEditRecordable,
  agentEditSidecarPath,
  beginAgentEdit,
  finishAgentEdit,
  latestAgentEdit,
  notePathFromAgentEditSidecar,
  parseAgentEditRecord,
  reviewedAgentEdit,
  serializeAgentEditRecord,
  type AgentEditRecord
} from './agent-edits'

const edit = (at: number, tool = 'replace_in_note') => ({ at, agent: 'Claude Code', tool })

/** Records one assistant edit that turns `current` into `next`. */
function record(existing: AgentEditRecord | null, current: string, next: string, at = 1) {
  return finishAgentEdit(beginAgentEdit(existing, current), next, edit(at))
}

describe('agent edit records', () => {
  it('keep the note from before the first edit, and the note after it', () => {
    const started = beginAgentEdit(null, 'a\nb\n')
    expect(started).toEqual({ version: 1, before: 'a\nb\n', after: null, edits: [], outside: [] })
    expect(finishAgentEdit(started, 'a\nB\n', edit(5))).toEqual({
      version: 1,
      before: 'a\nb\n',
      after: 'a\nB\n',
      edits: [edit(5)],
      outside: []
    })
  })

  it('stack a second edit onto the same record', () => {
    const first = record(null, 'a\nb\n', 'a\nB\n', 1)
    const second = record(first, 'a\nB\n', 'A\nB\n', 2)
    expect(second.before).toBe('a\nb\n')
    expect(second.after).toBe('A\nB\n')
    expect(second.outside).toEqual([])
    expect(second.edits.map((e) => e.at)).toEqual([1, 2])
  })

  it('note what someone else changed between two edits, without merging it', () => {
    const first = record(null, 'a\nb\nc\n', 'a\nB\nc\n', 1)
    // The user changes the last line, then the assistant edits the first one.
    const second = record(first, 'a\nB\nC\n', 'A\nB\nC\n', 2)
    expect(second.before).toBe('a\nb\nc\n')
    expect(second.outside).toEqual([{ from: 'a\nB\nc\n', to: 'a\nB\nC\n' }])
    // The review folds it in: only the assistant's two lines are compared.
    expect(agentEditOrigin(second)).toBe('a\nb\nC\n')
    expect(agentEditBaseline(second, 'A\nB\nC\n')).toBe('a\nb\nC\n')
  })

  it('show nothing to review once the assistant puts the note back', () => {
    const first = record(null, 'a\nb\n', 'a\nB\n')
    const second = record(first, 'a\nB\n', 'a\nb\n')
    expect(agentEditHasChanges(second, 'a\nb\n')).toBe(false)
  })

  it('cap the edit list but keep the texts', () => {
    let current: AgentEditRecord | null = null
    let text = 'line 0\n'
    for (let i = 1; i <= MAX_AGENT_EDIT_ENTRIES + 5; i++) {
      const next = `line ${i}\n`
      current = record(current, text, next, i)
      text = next
    }
    expect(current!.edits).toHaveLength(MAX_AGENT_EDIT_ENTRIES)
    expect(current!.before).toBe('line 0\n')
    expect(latestAgentEdit(current!)!.at).toBe(MAX_AGENT_EDIT_ENTRIES + 5)
  })

  it('start over rather than keep outside changes forever', () => {
    let current: AgentEditRecord = record(null, 'x\n', 'x1\n', 0)
    for (let i = 1; i <= MAX_AGENT_EDIT_OUTSIDE; i++) {
      current = record(current, `user ${i}\n`, `agent ${i}\n`, i)
    }
    expect(current.outside).toHaveLength(MAX_AGENT_EDIT_OUTSIDE)
    const fresh = record(current, 'user again\n', 'agent again\n', 99)
    expect(fresh.before).toBe('user again\n')
    expect(fresh.outside).toEqual([])
    expect(fresh.edits.map((e) => e.at)).toEqual([99])
  })
})

describe('agentEditBaseline', () => {
  const reviewed = record(null, 'a\nb\nc\n', 'a\nB\nc\n')

  it('is the old text when nothing else changed', () => {
    expect(agentEditBaseline(reviewed, 'a\nB\nc\n')).toBe('a\nb\nc\n')
    expect(agentEditHasChanges(reviewed, 'a\nB\nc\n')).toBe(true)
  })

  it('carries the user’s later lines, so only the assistant’s change is reviewed', () => {
    expect(agentEditBaseline(reviewed, 'a\nB\nc\nd\n')).toBe('a\nb\nc\nd\n')
  })

  it('leaves out a change the user has since rewritten', () => {
    expect(agentEditBaseline(reviewed, 'a\nX\nc\n')).toBe('a\nX\nc\n')
    expect(agentEditHasChanges(reviewed, 'a\nX\nc\n')).toBe(false)
  })

  it('falls back to the old text when the last edit never finished', () => {
    const unfinished = beginAgentEdit(reviewed, 'a\nB\nc\n')
    expect(unfinished.after).toBeNull()
    expect(agentEditBaseline(unfinished, 'a\nB\nc\nd\n')).toBe('a\nb\nc\n')
  })

  it('picks up a half-done review where it stopped', () => {
    const two = record(null, 'a\nb\nc\n', 'A\nb\nC\n')
    // The user accepted the first change and has not decided the second.
    const progress = reviewedAgentEdit(two, 'A\nb\nc\n', 'A\nb\nC\n')
    expect(agentEditBaseline(progress, 'A\nb\nC\n')).toBe('A\nb\nc\n')
    // Typing afterwards still stays out of the review.
    expect(agentEditBaseline(progress, 'A\nb\nC\nmore\n')).toBe('A\nb\nc\nmore\n')
  })
})

describe('agent edit files', () => {
  it('live beside the note’s comments and map back to the note', () => {
    expect(agentEditSidecarPath('.zennotes', 'inbox/Plan.md')).toBe('.zennotes/agent-edits/inbox/Plan.md.json')
    expect(notePathFromAgentEditSidecar('.zennotes', '.zennotes/agent-edits/inbox/Plan.md.json')).toBe('inbox/Plan.md')
    expect(notePathFromAgentEditSidecar('.zennotes', '.zennotes/comments/inbox/Plan.md.comments.json')).toBeNull()
  })

  it('round-trip through JSON and reject anything that is not a record', () => {
    const second = record(record(null, 'a\n', 'b\n', 7), 'c\n', 'd\n', 8)
    expect(parseAgentEditRecord(JSON.parse(serializeAgentEditRecord(second)))).toEqual(second)
    expect(parseAgentEditRecord(null)).toBeNull()
    expect(parseAgentEditRecord({ version: 2, before: 'a', after: null, edits: [] })).toBeNull()
    expect(parseAgentEditRecord({ version: 1, before: 1, after: null })).toBeNull()
    expect(parseAgentEditRecord({ version: 1, before: 'a', after: 3 })).toBeNull()
  })

  it('keep only well-formed entries', () => {
    const parsed = parseAgentEditRecord({
      version: 1,
      before: 'a',
      edits: [{ at: 1, agent: ' Codex ', tool: 'write_note' }, { at: 'x' }, null, { at: 2 }],
      outside: [{ from: 'a', to: 'b' }, { from: 1 }]
    })
    expect(parsed).toEqual({
      version: 1,
      before: 'a',
      after: null,
      edits: [
        { at: 1, agent: 'Codex', tool: 'write_note' },
        { at: 2, agent: 'Assistant', tool: '' }
      ],
      outside: [{ from: 'a', to: 'b' }]
    })
  })

  it('skip notes too large to keep copies of', () => {
    expect(agentEditRecordable('x'.repeat(MAX_AGENT_EDIT_NOTE_BYTES))).toBe(true)
    expect(agentEditRecordable('x'.repeat(MAX_AGENT_EDIT_NOTE_BYTES + 1))).toBe(false)
  })
})
