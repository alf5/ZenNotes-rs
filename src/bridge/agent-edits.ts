/**
 * Agent edit records (`.zennotes/agent-edits/<note>.json`) — the bridge half
 * of apps/desktop/src/main/agent-edit-files.ts and its IPC handlers.
 *
 * An assistant editing a note through upstream's MCP server records what the
 * note said before, so the change can be reviewed hunk by hunk. This build
 * ships no MCP server, but a vault is routinely shared with one (upstream's
 * desktop app, or the `zn mcp` CLI), so the records it leaves are reviewed
 * here the same way. Rust is the file layer; the record format is vendored
 * `@shared/agent-edits`, and nothing reaches disk that it did not accept.
 */

import { invoke } from '@tauri-apps/api/core'
import type { AgentEditRecord, AgentEditSummary } from '@zennotes/bridge-contract/ipc'
import {
  latestAgentEdit,
  parseAgentEditRecord,
  serializeAgentEditRecord
} from '@zennotes/shared-domain/agent-edits'

function parseText(text: string | null): AgentEditRecord | null {
  if (text == null) return null
  try {
    return parseAgentEditRecord(JSON.parse(text))
  } catch {
    return null
  }
}

/** Notes with assistant edits to review, newest edit first. Unreadable
 *  records are skipped. */
export async function listAgentEdits(): Promise<AgentEditSummary[]> {
  const files = await invoke<{ notePath: string; text: string }[]>('vault_list_agent_edits')
  const found: AgentEditSummary[] = []
  for (const file of files) {
    const record = parseText(file.text)
    if (!record) continue
    const latest = latestAgentEdit(record)
    found.push({
      notePath: file.notePath,
      agent: latest?.agent ?? 'Assistant',
      at: latest?.at ?? 0,
      edits: record.edits.length
    })
  }
  return found.sort((a, b) => b.at - a.at)
}

/** A note's record, or null when there is none or it is not readable. */
export async function readAgentEdit(relPath: string): Promise<AgentEditRecord | null> {
  if (typeof relPath !== 'string') return null
  return parseText(await invoke<string | null>('vault_read_agent_edit', { relPath }))
}

/** Save a review's progress, or delete the record with `null`. Only a
 *  well-formed record reaches the vault, and only inside
 *  `.zennotes/agent-edits` (Rust refuses any other path). */
export async function writeAgentEdit(
  relPath: string,
  record: AgentEditRecord | null
): Promise<void> {
  if (typeof relPath !== 'string') throw new Error('A note path is required')
  const parsed = record === null ? null : parseAgentEditRecord(record)
  if (record !== null && !parsed) throw new Error('Not an agent edit record')
  await invoke('vault_write_agent_edit', {
    relPath,
    text: parsed ? serializeAgentEditRecord(parsed) : null
  })
}
