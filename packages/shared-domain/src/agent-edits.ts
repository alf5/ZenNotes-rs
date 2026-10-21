import { mergeCloudSyncText } from './cloud-sync-merge'
import type { AgentEditEntry, AgentEditOutsideChange, AgentEditRecord } from './ipc'

export type { AgentEditEntry, AgentEditOutsideChange, AgentEditRecord }

/**
 * Agent edit records (docs/ideas/agent-edit-review.md). When an assistant
 * changes a note through the MCP server, the note's previous text is kept
 * beside it as `.zennotes/agent-edits/<note path>.json` until the user has
 * reviewed the change in the app: accepted it, or put the old text back.
 *
 * Two MCP engines write records: the TypeScript server in apps/desktop/src/mcp
 * and the Go `zn` (ZenNotes/tui), which is the one a managed install runs. So
 * the writing side is deliberately dumb, string comparisons and JSON, and
 * everything that needs a merge happens here, in the app:
 *
 * - `before`: the note before the first recorded edit.
 * - `after`: the note right after the latest edit, or `null` while an edit is
 *   being made. The record reaches disk before the edit, the way the workflow
 *   journal does, so a process that dies mid-edit leaves a record that shows
 *   more than the assistant did, never less.
 * - `outside`: changes someone else made between two assistant edits, as the
 *   note after one edit (`from`) and the note the next edit started from
 *   (`to`). A writer only appends here when they differ; it never merges.
 *
 * The app folds `outside` into `before` and then into the comparison text, so
 * the review shows only the assistant's changes: whatever the user typed is
 * theirs. Where the user rewrote lines the assistant had changed, the user's
 * version wins: those lines have been dealt with.
 */

export const AGENT_EDITS_DIR = 'agent-edits'
export const AGENT_EDITS_SUFFIX = '.json'
export const AGENT_EDIT_RECORD_VERSION = 1

/** Notes larger than this are not recorded: a record holds several copies. */
export const MAX_AGENT_EDIT_NOTE_BYTES = 1024 * 1024
/** How many edits a record lists; older entries drop off, the texts do not. */
export const MAX_AGENT_EDIT_ENTRIES = 50
/**
 * How many outside changes a record keeps apart. Past this, a writer starts
 * a fresh record from the note as it is: the earlier assistant edits are then
 * no longer offered for review, which forgets a review but changes no note.
 */
export const MAX_AGENT_EDIT_OUTSIDE = 20


/** Vault-relative posix path of the record for `notePath`. */
export function agentEditSidecarPath(internalDir: string, notePath: string): string {
  return `${internalDir}/${AGENT_EDITS_DIR}/${notePath}${AGENT_EDITS_SUFFIX}`
}

/** The note path a record path belongs to, or null for anything else. */
export function notePathFromAgentEditSidecar(internalDir: string, sidecarPath: string): string | null {
  const prefix = `${internalDir}/${AGENT_EDITS_DIR}/`
  if (!sidecarPath.startsWith(prefix) || !sidecarPath.endsWith(AGENT_EDITS_SUFFIX)) return null
  const notePath = sidecarPath.slice(prefix.length, -AGENT_EDITS_SUFFIX.length)
  return notePath.length > 0 ? notePath : null
}

export function agentEditRecordable(text: string): boolean {
  return new TextEncoder().encode(text).length <= MAX_AGENT_EDIT_NOTE_BYTES
}

function parseEntry(entry: unknown): AgentEditEntry[] {
  if (!entry || typeof entry !== 'object') return []
  const e = entry as Record<string, unknown>
  const at = typeof e['at'] === 'number' && Number.isFinite(e['at']) ? e['at'] : null
  const agent = typeof e['agent'] === 'string' ? e['agent'].trim().slice(0, 80) : ''
  const tool = typeof e['tool'] === 'string' ? e['tool'].trim().slice(0, 80) : ''
  return at === null ? [] : [{ at, agent: agent || 'Assistant', tool }]
}

/** A record read from disk, or null when it is missing or not a record. */
export function parseAgentEditRecord(raw: unknown): AgentEditRecord | null {
  if (!raw || typeof raw !== 'object') return null
  const value = raw as Record<string, unknown>
  if (value['version'] !== AGENT_EDIT_RECORD_VERSION) return null
  if (typeof value['before'] !== 'string') return null
  const after = value['after'] ?? null
  if (after !== null && typeof after !== 'string') return null
  const edits = Array.isArray(value['edits']) ? value['edits'] : []
  const outside = Array.isArray(value['outside']) ? value['outside'] : []
  return {
    version: AGENT_EDIT_RECORD_VERSION,
    before: value['before'],
    after,
    edits: edits.flatMap(parseEntry),
    outside: outside.flatMap((change): AgentEditOutsideChange[] => {
      if (!change || typeof change !== 'object') return []
      const c = change as Record<string, unknown>
      return typeof c['from'] === 'string' && typeof c['to'] === 'string'
        ? [{ from: c['from'], to: c['to'] }]
        : []
    })
  }
}

export function serializeAgentEditRecord(record: AgentEditRecord): string {
  return `${JSON.stringify(record)}\n`
}

/**
 * The record to write BEFORE an edit, given what is on disk (or null) and the
 * note as it is now. The Go engine mirrors this function and the next one;
 * keep them to string comparisons.
 */
export function beginAgentEdit(existing: AgentEditRecord | null, current: string): AgentEditRecord {
  const fresh: AgentEditRecord = {
    version: AGENT_EDIT_RECORD_VERSION,
    before: current,
    after: null,
    edits: [],
    outside: []
  }
  if (!existing) return fresh
  if (existing.after === null || existing.after === current) {
    return { ...existing, after: null }
  }
  if (existing.outside.length >= MAX_AGENT_EDIT_OUTSIDE) return fresh
  return {
    ...existing,
    after: null,
    outside: [...existing.outside, { from: existing.after, to: current }]
  }
}

/** The record to write AFTER an edit. */
export function finishAgentEdit(
  started: AgentEditRecord,
  current: string,
  entry: AgentEditEntry
): AgentEditRecord {
  return {
    ...started,
    after: current,
    edits: [...started.edits, entry].slice(-MAX_AGENT_EDIT_ENTRIES)
  }
}

/** `before` with every outside change folded in: the note without the assistant. */
export function agentEditOrigin(record: AgentEditRecord): string {
  let origin = record.before
  for (const change of record.outside) {
    origin = mergeCloudSyncText(change.from, change.to, origin).text
  }
  return origin
}

/**
 * The text the review compares the note against: the note as it is now,
 * without the assistant's changes. Without an `after` (an edit that never
 * finished), that is the origin itself.
 */
export function agentEditBaseline(record: AgentEditRecord, current: string): string {
  const origin = agentEditOrigin(record)
  if (record.after === null || record.after === current) return origin
  return mergeCloudSyncText(record.after, current, origin).text
}

/**
 * Whether anything is left to review. False once the assistant put its own
 * change back, or the user has since rewritten every line it touched; the
 * app then deletes the record instead of showing an empty review.
 */
export function agentEditHasChanges(record: AgentEditRecord, current: string): boolean {
  return agentEditBaseline(record, current) !== current
}

/**
 * The record a partly done review leaves behind: what has been accepted is
 * now part of the comparison text, and the note as it stands is the last
 * known state, so no outside change is left to fold.
 */
export function reviewedAgentEdit(
  record: AgentEditRecord,
  baseline: string,
  current: string
): AgentEditRecord {
  return { ...record, before: baseline, after: current, outside: [] }
}

/** The newest edit, for "Claude Code changed this note 4 minutes ago". */
export function latestAgentEdit(record: AgentEditRecord): AgentEditEntry | null {
  return record.edits.at(-1) ?? null
}
