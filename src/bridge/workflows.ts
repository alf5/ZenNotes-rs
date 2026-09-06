/**
 * The Workflows applier — port of apps/desktop/src/main/workflow-apply.ts.
 *
 * It lives in the webview rather than in Rust for the same reason the CSV
 * databases do: every transform it needs is already vendored. Op parsing, the
 * text transforms (`applyTextOp`) and the destination arithmetic (`moveTarget`,
 * `renameTarget`, `folderTarget`) come from `@shared/workflows/*`, so the rules
 * a plan is applied by are literally the rules it was planned by — one copy,
 * no drift. Rust supplies the file primitives (`db_*`), and with them the
 * containment check that no path from a plan can address outside the vault.
 *
 * The guarantees, unchanged from upstream:
 *
 *  - Every path is journalled with its PRE-RUN bytes before it is written, and
 *    the journal line hits the disk first (`db_append_text` fsyncs), so a
 *    process killed at any point leaves a journal covering every file it had
 *    begun to change.
 *  - A run that fails partway rolls back completely. A half-applied workflow is
 *    worse than none, because the author cannot tell which half landed.
 *  - Undo restores recorded bytes rather than replaying computed inverses.
 *    Fifteen op-specific inverses would be fifteen chances to corrupt a vault;
 *    recorded state is right by construction.
 *  - Apply, undo and delete are serialized, so the Undo toast cannot race a
 *    second run into interleaving their read-journal-write sequences.
 */

import { invoke } from '@tauri-apps/api/core'
import type {
  ApplyWorkflowInput,
  WorkflowJournalEntry,
  WorkflowRunReceipt,
  WorkflowRunSummary,
  WorkflowUndoResult
} from '@zennotes/bridge-contract/workflows'
import { WORKFLOWS_REL_DIR } from '@zennotes/shared-domain/workflows-view'
import { applyTextOp } from '@zennotes/shared-domain/workflows/apply-ops'
import {
  folderTarget,
  joinRel,
  moveTarget,
  normalizeRel,
  noteExtensionOf,
  relBasename,
  relDirname,
  renameTarget,
  stripNoteExtension,
  type SystemFolderDirs
} from '@zennotes/shared-domain/workflows/paths'
import type { WorkflowOp } from '@zennotes/shared-domain/workflows/types'

/* -------------------------------------------------------------------------- */
/*  Layout                                                                    */
/* -------------------------------------------------------------------------- */

/** Run ledgers live beside the workflows that produced them, in a dot
 *  directory so `listWorkflows` (which skips dot entries) never mistakes one
 *  for a workflow. */
const RUNS_REL_DIR = `${WORKFLOWS_REL_DIR}/.runs`

/** Bumped when the on-disk ledger shape changes; older files are then ignored. */
const LEDGER_VERSION = 1

/** A ledger carries a full copy of every touched file's previous bytes, so the
 *  history is bounded rather than growing forever. Pruning only ever forgets an
 *  undo option, it never changes a note. */
const MAX_RETAINED_RUNS = 100

/** How much disk the retained ledgers may take, all together. The count cap
 *  alone is not a size cap: one run over a whole vault stores a pre-image of
 *  every note it touched. */
const MAX_RETAINED_RUN_BYTES = 50 * 1024 * 1024

/** The internal directory a workflow must never write into. */
const INTERNAL_DIR = '.zennotes'

/** Suffix of the crash journal a run keeps while it is applying. Not `.json`,
 *  so the history readers cannot mistake one for a finished run. */
const RUN_JOURNAL_SUFFIX = '.journal.jsonl'

/** Ops with no possible inverse: they never reach the filesystem, and the
 *  renderer is where a clipboard and a notification actually exist. */
const IRREVERSIBLE_OP_KINDS: ReadonlySet<string> = new Set(['notify', 'clipboard'])

/** Text ops that act on a note the plan saw on a wire, rather than on a path
 *  the author typed. Only these demand that their target still exists. */
const PER_NOTE_TEXT_OPS: ReadonlySet<string> = new Set([
  'set-frontmatter',
  'add-tag',
  'remove-tag',
  'append',
  'prepend',
  'apply-template'
])

/** One applied run, exactly as it is persisted. */
interface WorkflowRunLedger {
  version: number
  runId: string
  workflowId: string
  startedAt: number
  finishedAt: number
  applied: number
  irreversible: number
  paths: string[]
  ops: WorkflowOp[]
  journal: WorkflowJournalEntry[]
  /** Content hash of what the run LEFT at each path (null where it removed the
   *  file), so undo can tell a file it wrote from one that has since changed. */
  hashes: Record<string, string | null>
  undone: boolean
  undoneAt?: number
  rolledBack?: { reason: string }
  interrupted?: { reason: string }
}

/* -------------------------------------------------------------------------- */
/*  Vault file primitives                                                     */
/* -------------------------------------------------------------------------- */

const readText = (relPath: string): Promise<string | null> =>
  invoke<string | null>('db_read_text', { relPath })
const writeThroughLinks = (relPath: string, text: string): Promise<void> =>
  invoke('db_write_through_links', { relPath, text })
const removeThroughLinks = (relPath: string): Promise<void> =>
  invoke('db_remove_through_links', { relPath })
const writeText = (relPath: string, text: string): Promise<void> =>
  invoke('db_write_text', { relPath, text })
const deleteFile = (relPath: string): Promise<void> => invoke('db_delete_file', { relPath })
const appendText = (relPath: string, text: string): Promise<void> =>
  invoke('db_append_text', { relPath, text })
const exists = (relPath: string): Promise<boolean> => invoke<boolean>('db_exists', { relPath })
const listDir = (relPath: string): Promise<string[]> => invoke<string[]>('db_list_dir', { relPath })
const fileSize = (relPath: string): Promise<number> => invoke<number>('db_file_size', { relPath })

/**
 * Refuse a vault-relative path a workflow may not write, before anything is
 * written. Rust's `resolve_safe` is the backstop on every primitive above; this
 * is the copy that can abort the whole run up front, so there is nothing to
 * roll back. Textual on purpose, exactly like upstream: a symlinked note whose
 * target sits outside the vault is a note the app already saves through, and
 * canonicalising here would refuse it.
 */
function resolveVaultRel(rel: string): string {
  const raw = String(rel ?? '')
  if (/^[/\\]/.test(raw) || /^[A-Za-z]:/.test(raw)) {
    throw new Error(`Refusing an absolute workflow path: ${raw}`)
  }
  const clean = normalizeRel(raw)
  const segments = clean.split('/')
  if (segments.includes('..')) {
    throw new Error(`Refusing a workflow path that climbs out of the vault: ${raw}`)
  }
  if ((segments[0] ?? '').toLowerCase() === INTERNAL_DIR) {
    throw new Error(`Refusing a workflow path inside ${INTERNAL_DIR}: ${raw}`)
  }
  if (!clean) throw new Error('Refusing an empty workflow path')
  return clean
}

/** Every path an op will touch, so containment can be checked before the run. */
function opTargets(op: WorkflowOp, dirs: SystemFolderDirs): string[] {
  switch (op.kind) {
    case 'notify':
    case 'clipboard':
      return []
    case 'move':
      return [op.path, moveTarget(normalizeRel(op.path), op.to)]
    case 'rename':
      return [op.path, renameTarget(normalizeRel(op.path), op.to)]
    case 'archive':
      return [op.path, folderTarget('archive', normalizeRel(op.path), dirs)]
    case 'trash':
      return [op.path, folderTarget('trash', normalizeRel(op.path), dirs)]
    default:
      return [op.path]
  }
}

/**
 * A destination that is not already taken, matching `uniqueTitle` in the vault
 * (`X.md`, `X 2.md`, `X 3.md`). A move that clobbered an existing note would
 * still be undoable, but a second run without an undo would silently merge two
 * notes into one and nothing in the dry run warned about it.
 */
async function uniqueRel(rel: string): Promise<string> {
  const ext = noteExtensionOf(rel)
  const stem = joinRel(relDirname(rel), stripNoteExtension(relBasename(rel)))
  let candidate = rel
  // A ceiling rather than `while (true)`: a directory that answers "exists" to
  // everything must fail loudly instead of spinning inside a run that holds a
  // half-written journal.
  for (let n = 2; n < 1000; n += 1) {
    if (!(await exists(candidate))) return candidate
    candidate = `${stem} ${n}${ext}`
  }
  throw new Error(`Cannot find a free destination for ${rel}`)
}

async function hashText(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text)
  const digest = await crypto.subtle.digest('SHA-256', bytes)
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}

/* -------------------------------------------------------------------------- */
/*  Op validation                                                             */
/* -------------------------------------------------------------------------- */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function str(record: Record<string, unknown>, key: string): string | null {
  const value = record[key]
  return typeof value === 'string' ? value : null
}

/**
 * Turn an untrusted value into a WorkflowOp, or null.
 *
 * The ops arrive from the renderer's planner, but they also arrive from a
 * ledger read off disk, which is a file anything could have written. Every
 * field is checked rather than cast.
 */
export function parseWorkflowOp(value: unknown): WorkflowOp | null {
  if (!isRecord(value)) return null
  const kind = str(value, 'kind')
  if (!kind) return null
  const path = str(value, 'path')
  switch (kind) {
    case 'notify': {
      const message = str(value, 'message')
      return message === null ? null : { kind, message }
    }
    case 'clipboard': {
      const text = str(value, 'text')
      return text === null ? null : { kind, text }
    }
    case 'set-frontmatter': {
      const field = str(value, 'field')
      const fieldValue = str(value, 'value')
      return path && field !== null && fieldValue !== null
        ? { kind, path, field, value: fieldValue }
        : null
    }
    case 'add-tag':
    case 'remove-tag': {
      const tag = str(value, 'tag')
      return path && tag !== null ? { kind, path, tag } : null
    }
    case 'move':
    case 'rename': {
      const to = str(value, 'to')
      return path && to !== null ? { kind, path, to } : null
    }
    case 'append':
    case 'prepend': {
      const text = str(value, 'text')
      return path && text !== null ? { kind, path, text } : null
    }
    case 'write-section': {
      const heading = str(value, 'heading')
      const text = str(value, 'text')
      return path && heading !== null && text !== null ? { kind, path, heading, text } : null
    }
    case 'write-note': {
      const text = str(value, 'text')
      return path && text !== null ? { kind, path, text } : null
    }
    case 'create-note': {
      const body = str(value, 'body')
      return path && body !== null ? { kind, path, body } : null
    }
    case 'apply-template': {
      const template = str(value, 'template')
      return path && template !== null ? { kind, path, template } : null
    }
    case 'archive':
    case 'trash':
      return path ? { kind, path } : null
    default:
      return null
  }
}

function parseWorkflowOps(values: unknown[]): WorkflowOp[] {
  const ops: WorkflowOp[] = []
  for (const value of values) {
    const op = parseWorkflowOp(value)
    // A plan the applier cannot read is a plan it must not half-run: the step
    // it skipped is exactly the one nobody would notice was missing.
    if (!op) throw new Error('This workflow plan contains a step ZenNotes does not recognise.')
    ops.push(op)
  }
  return ops
}

/* -------------------------------------------------------------------------- */
/*  Run ids and ledger paths                                                  */
/* -------------------------------------------------------------------------- */

let runSequence = 0

/**
 * `<startedAt>-<sequence>-<ops hash>`. The 13-digit millisecond timestamp
 * leads, so ledger filenames sort lexicographically in the order they ran. The
 * ops hash makes the id a fingerprint of what the run intended.
 */
async function makeRunId(startedAt: number, workflowId: string, ops: WorkflowOp[]): Promise<string> {
  runSequence = (runSequence + 1) % 1000
  const seq = String(runSequence).padStart(3, '0')
  const digest = (await hashText(`${workflowId}\n${JSON.stringify(ops)}`)).slice(0, 8)
  return `${startedAt}-${seq}-${digest}`
}

/** A run id no ledger has taken, so a run can never overwrite another's. */
async function allocateRunId(
  startedAt: number,
  workflowId: string,
  ops: WorkflowOp[]
): Promise<string> {
  for (let attempt = 0; attempt < 1000; attempt += 1) {
    const runId = await makeRunId(startedAt, workflowId, ops)
    if (!(await exists(ledgerRelFor(runId)))) return runId
  }
  throw new Error('Cannot allocate a workflow run id')
}

function ledgerRelFor(runId: string): string {
  return `${RUNS_REL_DIR}/${runId}.json`
}

function journalRelFor(runId: string): string {
  return `${RUNS_REL_DIR}/${runId}${RUN_JOURNAL_SUFFIX}`
}

/**
 * Resolve a ledger file for a run id that came from the renderer. The id names
 * a file, so it is checked as one: charset first, and no separators, so the
 * result cannot leave the runs directory.
 */
function resolveLedgerRel(runId: string): string {
  if (!/^[A-Za-z0-9._-]+$/.test(runId) || runId.includes('..')) {
    throw new Error(`Invalid workflow run id: ${runId}`)
  }
  return ledgerRelFor(runId)
}

/* -------------------------------------------------------------------------- */
/*  The crash journal                                                         */
/* -------------------------------------------------------------------------- */

/**
 * The run's journal as it is being written, one line at a time.
 *
 * A `.jsonl` rather than a rewritten `.json`: an append is a single fsynced
 * write the kernel either has or has not taken, so a process that dies
 * mid-append loses at most its last line, while rewriting a whole document per
 * entry would put every earlier entry at risk on every op. The first line is
 * the run's header; every line after it is one journal entry, in the order the
 * run touched the paths.
 */
interface RunJournalFile {
  rel: string
  header: string
  started: boolean
}

interface RunIdentity {
  runId: string
  workflowId: string
  startedAt: number
  irreversible: number
  ops: WorkflowOp[]
}

function newRunJournalFile(run: RunIdentity): RunJournalFile {
  return {
    rel: journalRelFor(run.runId),
    header: JSON.stringify({
      runId: run.runId,
      workflowId: run.workflowId,
      startedAt: run.startedAt,
      irreversible: run.irreversible,
      ops: run.ops
    }),
    started: false
  }
}

async function appendJournalEntry(
  file: RunJournalFile,
  entry: WorkflowJournalEntry
): Promise<void> {
  if (!file.started) {
    await appendText(file.rel, `${file.header}\n`)
    file.started = true
  }
  await appendText(file.rel, `${JSON.stringify(entry)}\n`)
}

async function discardRunJournal(file: RunJournalFile): Promise<void> {
  if (!file.started) return
  await deleteFile(file.rel).catch(() => {})
  file.started = false
}

interface ParsedRunJournal {
  workflowId: string
  startedAt: number
  irreversible: number
  ops: WorkflowOp[]
  journal: WorkflowJournalEntry[]
}

/**
 * Read a crash journal back. A truncated last line is dropped rather than
 * failing the parse: it is exactly the line a killed process was writing, and
 * every line before it is still a true record.
 */
async function readRunJournalFile(rel: string): Promise<ParsedRunJournal | null> {
  const raw = await readText(rel)
  if (raw === null) return null
  const lines = raw.split('\n').filter((line) => line.length > 0)
  if (lines.length === 0) return null
  let header: unknown
  try {
    header = JSON.parse(lines[0] as string)
  } catch {
    return null
  }
  if (!isRecord(header)) return null
  const journal: WorkflowJournalEntry[] = []
  for (const line of lines.slice(1)) {
    try {
      const entry: unknown = JSON.parse(line)
      if (!isRecord(entry)) continue
      const entryPath = entry.path
      const before = entry.before
      if (typeof entryPath !== 'string') continue
      if (typeof before !== 'string' && before !== null) continue
      journal.push({ path: entryPath, before })
    } catch {
      // The truncated tail of a killed write.
    }
  }
  return {
    workflowId: typeof header.workflowId === 'string' ? header.workflowId : 'unknown',
    startedAt: typeof header.startedAt === 'number' ? header.startedAt : 0,
    irreversible: typeof header.irreversible === 'number' ? header.irreversible : 0,
    ops: Array.isArray(header.ops)
      ? header.ops.map(parseWorkflowOp).filter((op): op is WorkflowOp => op !== null)
      : [],
    journal
  }
}

/* -------------------------------------------------------------------------- */
/*  Applying                                                                  */
/* -------------------------------------------------------------------------- */

interface RunState {
  systemFolderDirs: SystemFolderDirs
  /** Journal key to the entry, in the order the run touched the paths. Written
   *  only when absent — the first-touch-wins rule, so the entry describes the
   *  state before the RUN, not before the latest op. */
  journal: Map<string, WorkflowJournalEntry>
  journalFile: RunJournalFile
  /** Journal key to the path as the run named it and the hash of what it left
   *  there (null where it removed the file). */
  written: Map<string, { path: string; hash: string | null }>
  /** Destination the plan promised to where the file actually landed, for the
   *  one case where they differ: `uniqueRel` had to suffix around a collision. */
  redirects: Map<string, string>
}

/**
 * How the journal recognises two paths as the same file. This build targets
 * Linux, where `Inbox/A.md` and `inbox/a.md` really are two files, so the key
 * is the path itself. (Upstream folds case on darwin and win32, where
 * journalling them separately would break first-touch-wins.)
 */
function journalKey(rel: string): string {
  return rel
}

/**
 * Record a path's pre-run bytes, on disk before it is recorded in memory.
 * The order is the durability guarantee: every caller awaits this before the
 * write it describes.
 */
async function journalTouch(state: RunState, rel: string, before: string | null): Promise<void> {
  const key = journalKey(rel)
  if (state.journal.has(key)) return
  const entry: WorkflowJournalEntry = { path: rel, before }
  await appendJournalEntry(state.journalFile, entry)
  state.journal.set(key, entry)
}

/** Note what the run left at a path. The spelling of the first touch wins. */
function recordWritten(state: RunState, rel: string, hash: string | null): void {
  const key = journalKey(rel)
  const seen = state.written.get(key)
  if (seen) seen.hash = hash
  else state.written.set(key, { path: rel, hash })
}

/**
 * Where an op's stated path actually lives right now. The stated path wins
 * whenever the file is still there, so a redirect can never hijack an op aimed
 * at a real note; the redirect is followed only into the gap this run created.
 */
async function resolveLiveRel(state: RunState, rel: string): Promise<string> {
  const stated = normalizeRel(rel)
  const via = state.redirects.get(stated)
  if (via === undefined) return stated
  if (await exists(stated)) return stated
  return via
}

/** One content transform: read, journal, write. Identical output writes nothing. */
async function applyTextOpToVault(state: RunState, op: WorkflowOp): Promise<void> {
  const rel = resolveVaultRel(await resolveLiveRel(state, (op as { path: string }).path))
  const live = await readText(rel)
  // `create-note` MEANS create. Its transform is a whole-file write that
  // ignores the previous bytes, so letting it land on an existing note would
  // silently replace it. Refusing here fails the run, which rolls the whole
  // thing back. Replacing a note on purpose is `write`.
  if (op.kind === 'create-note' && live !== null) {
    throw new Error(
      `create-note would replace the existing note ${rel}. Use write to replace a note on purpose.`
    )
  }
  // The mirror image, for ops that act on a wire note: a target that has
  // vanished means the plan describes a vault that no longer exists. Applying
  // the transform to '' would materialize a phantom note at the abandoned path.
  if (live === null && PER_NOTE_TEXT_OPS.has(op.kind)) {
    throw new Error(
      `Cannot ${op.kind} ${rel}: the note is missing (moved or removed earlier in this run?)`
    )
  }
  const next = applyTextOp(live ?? '', op)
  // `applyTextOp` answers null only for an op that is not a text op. Reaching
  // that here would mean this module and the shared one disagree about which
  // side of the line an op sits on, and a silently skipped write is exactly the
  // failure this feature exists to make impossible.
  if (next === null) throw new Error(`Workflow op ${op.kind} is not a text op`)
  // An op that changes nothing (adding a tag the note already carries) should
  // not touch the file: an mtime bump is a sync round trip and a watcher event
  // for a change that did not happen.
  if (live !== null && next === live) return
  await journalTouch(state, rel, live)
  await writeThroughLinks(rel, next)
  recordWritten(state, rel, await hashText(next))
}

/** One path change. Journals both ends, which is what makes undo need no inverse. */
async function movePathInVault(
  state: RunState,
  kind: WorkflowOp['kind'],
  fromRel: string,
  nominalRel: string,
  promisedRel: string
): Promise<void> {
  const from = resolveVaultRel(fromRel)
  const live = await readText(from)
  // The plan was made from a snapshot; if the note has since gone, the rest of
  // the plan is describing a vault that no longer exists.
  if (live === null) throw new Error(`Cannot ${kind} ${from}: the note is missing`)
  const nominal = resolveVaultRel(nominalRel)
  // Already where the op wants it (trashing something in the trash). Not an
  // error, and nothing to journal.
  if (nominal === from) return

  const to = await uniqueRel(nominal)
  await journalTouch(state, from, live)
  // `uniqueRel` just said this path is free, but it is read rather than assumed
  // null: another process can create a file inside that window, and journalling
  // it as "did not exist" would turn undo into a delete of somebody's note.
  await journalTouch(state, to, await readText(to))
  const parent = relDirname(to)
  if (parent) await invoke('db_mkdir', { relPath: parent })
  await invoke('db_rename', { oldRel: from, newRel: to })
  recordWritten(state, from, null)
  recordWritten(state, to, await hashText(live))
  // Later ops in this plan name the destination the engine promised; when the
  // vault forced a different one, leave a forwarding address.
  const promised = normalizeRel(promisedRel)
  if (to !== promised) state.redirects.set(promised, to)
}

async function applyOp(state: RunState, op: WorkflowOp): Promise<void> {
  switch (op.kind) {
    // Neither reaches the filesystem, so neither is journalled. They are
    // counted as irreversible on the receipt and performed by the renderer.
    case 'notify':
    case 'clipboard':
      return
    // Path ops resolve their source through the run's forwarding addresses,
    // then aim where the ACTUAL file's name says (an earlier collision suffix
    // must survive a later move), while the redirect they record is keyed by
    // the destination the ENGINE promised, because that is the name later ops
    // will use.
    case 'move': {
      const from = await resolveLiveRel(state, op.path)
      return await movePathInVault(
        state,
        op.kind,
        from,
        moveTarget(from, op.to),
        moveTarget(normalizeRel(op.path), op.to)
      )
    }
    case 'rename': {
      const from = await resolveLiveRel(state, op.path)
      return await movePathInVault(
        state,
        op.kind,
        from,
        renameTarget(from, op.to),
        renameTarget(normalizeRel(op.path), op.to)
      )
    }
    case 'archive':
    case 'trash': {
      const from = await resolveLiveRel(state, op.path)
      return await movePathInVault(
        state,
        op.kind,
        from,
        folderTarget(op.kind, from, state.systemFolderDirs),
        folderTarget(op.kind, normalizeRel(op.path), state.systemFolderDirs)
      )
    }
    default:
      return await applyTextOpToVault(state, op)
  }
}

/* -------------------------------------------------------------------------- */
/*  Restoring                                                                 */
/* -------------------------------------------------------------------------- */

interface RestoreFailure {
  path: string
  message: string
}

function describeFailures(failures: RestoreFailure[]): string {
  return failures.map((failure) => `${failure.path} (${failure.message})`).join('; ')
}

/**
 * Put every journalled path back the way the run found it. Returns what it
 * could not restore rather than throwing: the caller is already handling a
 * failure and needs the full list, and a rollback that stops at the first
 * problem leaves more of the vault wrong than one that carries on.
 */
async function restoreEntries(
  entries: Iterable<WorkflowJournalEntry>
): Promise<{ restored: number; failures: RestoreFailure[] }> {
  const failures: RestoreFailure[] = []
  let restored = 0
  for (const { path: rel, before } of entries) {
    try {
      // Re-validated on the way back out. At rollback time these paths came
      // from this run, but undo replays the same code over a file read off
      // disk, and that file must never be able to point a write anywhere.
      const safe = resolveVaultRel(rel)
      // A path is journalled BEFORE its write is attempted, so a run that
      // failed mid-way has journalled paths it never actually changed.
      // Restoring those is pointless at best: whatever failed the write fails
      // the restore too, and the rollback then reports ROLLBACK INCOMPLETE over
      // a file that was never touched.
      if ((await readText(safe)) === before) {
        restored += 1
        continue
      }
      if (before === null) {
        // Through a symlink, what the run created is the TARGET file: the link
        // itself was there before the run, so putting the vault back means
        // leaving it there, pointing at nothing again.
        await removeThroughLinks(safe)
      } else {
        await writeThroughLinks(safe, before)
      }
      restored += 1
    } catch (err) {
      failures.push({ path: rel, message: messageOf(err) })
    }
  }
  return { restored, failures }
}

/* -------------------------------------------------------------------------- */
/*  Ledgers                                                                   */
/* -------------------------------------------------------------------------- */

async function writeLedger(ledger: WorkflowRunLedger): Promise<void> {
  await writeText(ledgerRelFor(ledger.runId), JSON.stringify(ledger))
}

function parseNumber(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback
}

function parseJournal(value: unknown): WorkflowJournalEntry[] {
  if (!Array.isArray(value)) return []
  const entries: WorkflowJournalEntry[] = []
  for (const item of value) {
    if (!isRecord(item)) continue
    const entryPath = item.path
    const before = item.before
    if (typeof entryPath !== 'string') continue
    if (typeof before !== 'string' && before !== null) continue
    entries.push({ path: entryPath, before })
  }
  return entries
}

/**
 * Read one ledger, tolerating a file a newer version wrote fields into. The
 * journal is validated strictly (it is what undo writes from) while the
 * reporting fields fall back, because a run whose `applied` count is unreadable
 * is still a run the user must be able to undo.
 */
async function readLedger(rel: string): Promise<WorkflowRunLedger | null> {
  const raw = await readText(rel)
  if (raw === null) return null
  const parsed: unknown = JSON.parse(raw)
  if (!isRecord(parsed)) throw new Error('Run ledger is not an object')
  const runId = parsed.runId
  if (typeof runId !== 'string' || !runId) throw new Error('Run ledger has no run id')
  const hashes: Record<string, string | null> = {}
  if (isRecord(parsed.hashes)) {
    for (const [key, hash] of Object.entries(parsed.hashes)) {
      if (typeof hash === 'string' || hash === null) hashes[key] = hash
    }
  }
  const ledger: WorkflowRunLedger = {
    version: parseNumber(parsed.version, LEDGER_VERSION),
    runId,
    workflowId: typeof parsed.workflowId === 'string' ? parsed.workflowId : 'unknown',
    startedAt: parseNumber(parsed.startedAt, 0),
    finishedAt: parseNumber(parsed.finishedAt, 0),
    applied: parseNumber(parsed.applied, 0),
    irreversible: parseNumber(parsed.irreversible, 0),
    paths: Array.isArray(parsed.paths)
      ? parsed.paths.filter((item): item is string => typeof item === 'string')
      : [],
    ops: Array.isArray(parsed.ops)
      ? parsed.ops.map(parseWorkflowOp).filter((op): op is WorkflowOp => op !== null)
      : [],
    journal: parseJournal(parsed.journal),
    hashes,
    undone: parsed.undone === true
  }
  if (typeof parsed.undoneAt === 'number') ledger.undoneAt = parsed.undoneAt
  const rolledBack = parsed.rolledBack
  if (isRecord(rolledBack) && typeof rolledBack.reason === 'string') {
    ledger.rolledBack = { reason: rolledBack.reason }
  }
  const interrupted = parsed.interrupted
  if (isRecord(interrupted) && typeof interrupted.reason === 'string') {
    ledger.interrupted = { reason: interrupted.reason }
  }
  return ledger
}

/**
 * Keep the history bounded, by count and by total size, so a scheduled workflow
 * cannot let run history quietly become the largest thing in `.zennotes`.
 */
async function pruneRunLedgers(): Promise<void> {
  try {
    const names = (await listDir(RUNS_REL_DIR))
      .filter((n) => n.toLowerCase().endsWith('.json'))
      .sort()
    const excess = new Set(names.slice(0, Math.max(0, names.length - MAX_RETAINED_RUNS)))
    const survivors = names.filter((name) => !excess.has(name))
    // Newest backwards, keeping runs while they fit. The most recent is kept
    // whatever it weighs: deleting the ledger just written would take the undo
    // away from the very run the user is being shown.
    let used = 0
    for (let i = survivors.length - 1; i >= 0; i -= 1) {
      const name = survivors[i] as string
      used += await fileSize(`${RUNS_REL_DIR}/${name}`)
      if (used > MAX_RETAINED_RUN_BYTES && i < survivors.length - 1) {
        for (let older = i; older >= 0; older -= 1) excess.add(survivors[older] as string)
        break
      }
    }
    for (const name of excess) await deleteFile(`${RUNS_REL_DIR}/${name}`)
  } catch (err) {
    // Losing the prune is harmless; failing a completed run over it is not.
    console.warn('[workflows] could not prune old run ledgers:', err)
  }
}

/* -------------------------------------------------------------------------- */
/*  Recovery                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * Turn every crash journal left by a process that did not survive its run into
 * an undoable ledger. Runs before a new run adds a journal of its own, so an
 * interrupted run is in the history rather than being taken for this one's
 * leftovers.
 */
async function recoverInterruptedRunsNow(): Promise<string[]> {
  let names: string[]
  try {
    names = await listDir(RUNS_REL_DIR)
  } catch {
    return [] // no runs directory: nothing has ever run here
  }
  const recovered: string[] = []
  for (const name of names) {
    if (!name.toLowerCase().endsWith(RUN_JOURNAL_SUFFIX)) continue
    const runId = name.slice(0, name.length - RUN_JOURNAL_SUFFIX.length)
    const rel = `${RUNS_REL_DIR}/${name}`
    try {
      // A ledger means the run reached its own ending and said what happened;
      // the journal simply outlived the delete that should have followed it.
      if (await exists(ledgerRelFor(runId))) {
        await deleteFile(rel)
        continue
      }
      const parsed = await readRunJournalFile(rel)
      // Nothing journalled means the run died before its first write, so the
      // vault is untouched and there is nothing to offer.
      if (!parsed || parsed.journal.length === 0) {
        await deleteFile(rel)
        continue
      }
      await writeLedger({
        version: LEDGER_VERSION,
        runId,
        workflowId: parsed.workflowId,
        startedAt: parsed.startedAt,
        finishedAt: Date.now(),
        // Unknowable from here: the journal says which paths were AT RISK, not
        // which writes landed. Zero and the empty hash map are the honest
        // answers, and `interrupted` is what tells the reader why.
        applied: 0,
        irreversible: parsed.irreversible,
        paths: parsed.journal.map((entry) => entry.path),
        ops: parsed.ops,
        journal: parsed.journal,
        hashes: {},
        undone: false,
        interrupted: {
          reason:
            'ZenNotes stopped while this run was still applying, so part of it may have landed. ' +
            'Undo restores every file it had recorded.'
        }
      })
      await deleteFile(rel)
      recovered.push(runId)
    } catch (err) {
      // Left in place on purpose: an unreadable or unwritable recovery is worth
      // retrying next time, and deleting it would throw away the only copy of
      // those pre-run bytes.
      console.warn(`[workflows] could not recover interrupted run ${runId}:`, err)
    }
  }
  return recovered
}

/* -------------------------------------------------------------------------- */
/*  One run at a time                                                         */
/* -------------------------------------------------------------------------- */

/**
 * Apply, undo and history-delete are serialized. The Undo toast racing a second
 * run would otherwise interleave their read-journal-write sequences: one run
 * journals bytes another is about to replace, and a rollback then restores
 * pre-run bytes over committed writes while both receipts report success.
 *
 * A promise chain, matching the shape upstream uses. It covers this webview;
 * two windows on one vault are serialized only within each. That is a narrower
 * guarantee than upstream's (which holds the lock in the single main process),
 * and it is recorded in GAP-ANALYSIS.md rather than papered over.
 */
let runQueue: Promise<unknown> = Promise.resolve()

function withRunLock<T>(task: () => Promise<T>): Promise<T> {
  const next = runQueue.then(task)
  // Settled-only, so one failed run cannot wedge the chain or resurface its
  // error in the next caller; `next` still carries the real rejection.
  runQueue = next.then(
    () => undefined,
    () => undefined
  )
  return next
}

/* -------------------------------------------------------------------------- */
/*  The bridge surface                                                        */
/* -------------------------------------------------------------------------- */

interface VaultSettingsShape {
  systemFolderPaths?: SystemFolderDirs
}

async function systemFolderDirsOf(): Promise<SystemFolderDirs> {
  try {
    const settings = await invoke<VaultSettingsShape>('vault_get_settings')
    return settings?.systemFolderPaths ?? {}
  } catch {
    // A vault whose settings cannot be read still has the default folders.
    return {}
  }
}

export function applyWorkflow(input: ApplyWorkflowInput): Promise<WorkflowRunReceipt> {
  return withRunLock(() => applyWorkflowNow(input))
}

async function applyWorkflowNow(input: ApplyWorkflowInput): Promise<WorkflowRunReceipt> {
  const startedAt = Date.now()
  const workflowId = input.workflowId.trim() || 'unknown'
  const ops = parseWorkflowOps(input.ops)
  const irreversible = ops.filter((op) => IRREVERSIBLE_OP_KINDS.has(op.kind)).length

  // Containment first, for every path in the plan including the destinations
  // this module computes. One traversal attempt aborts the run before a single
  // byte is written, so there is nothing to roll back.
  const systemFolderDirs = await systemFolderDirsOf()
  for (const op of ops) {
    for (const target of opTargets(op, systemFolderDirs)) resolveVaultRel(target)
  }

  // Before this run adds a journal of its own.
  await recoverInterruptedRunsNow()

  const runId = await allocateRunId(startedAt, workflowId, ops)
  const run: RunIdentity = { runId, workflowId, startedAt, irreversible, ops }
  const state: RunState = {
    systemFolderDirs,
    journal: new Map(),
    journalFile: newRunJournalFile(run),
    written: new Map(),
    redirects: new Map()
  }
  let applied = 0

  try {
    for (const op of ops) {
      await applyOp(state, op)
      // `notify` and `clipboard` did not run here, so they are not counted as
      // applied; they are already declared in `irreversible`.
      if (!IRREVERSIBLE_OP_KINDS.has(op.kind)) applied += 1
    }
  } catch (err) {
    return await rollBackRun(run, state, messageOf(err))
  }

  const paths = [...state.written.values()].map((entry) => entry.path)
  // A run with no ops at all left no record worth keeping; anything that ran
  // gets a ledger, even one whose journal is empty (a notify-only run), so the
  // history shows it happened and shows it cannot be undone.
  if (ops.length > 0) {
    try {
      await writeLedger({
        ...ledgerBase(run, state),
        finishedAt: Date.now(),
        applied,
        paths,
        undone: false
      })
    } catch (err) {
      // A run that cannot be recorded is a run that cannot be undone, and every
      // promise this feature makes rests on undo. So the journal, still in
      // memory and still correct, is spent putting the vault back.
      return await rollBackRun(
        run,
        state,
        `The run could not be recorded, so it was not kept (${messageOf(err)})`
      )
    }
    // The ledger now holds the same journal, so the crash copy has nothing
    // left to protect.
    await discardRunJournal(state.journalFile)
    await pruneRunLedgers()
  } else {
    await discardRunJournal(state.journalFile)
  }

  return { runId, workflowId, startedAt, applied, paths, irreversible }
}

function ledgerBase(
  run: RunIdentity,
  state: RunState
): Omit<WorkflowRunLedger, 'finishedAt' | 'applied' | 'paths' | 'undone'> {
  return {
    version: LEDGER_VERSION,
    runId: run.runId,
    workflowId: run.workflowId,
    startedAt: run.startedAt,
    irreversible: run.irreversible,
    ops: run.ops,
    journal: [...state.journal.values()],
    hashes: Object.fromEntries(
      [...state.written.values()].map((entry) => [entry.path, entry.hash] as const)
    )
  }
}

/**
 * Unwind everything the run wrote and report it, whatever went wrong.
 *
 * The clean case leaves no ledger: the vault is exactly as it was found, so a
 * run in the history offering an undo would be describing changes that are not
 * there. The incomplete case is the opposite: the journal is now the only
 * record of files still holding this run's bytes, so it is persisted and the
 * reason says so in as many words.
 */
async function rollBackRun(
  run: RunIdentity,
  state: RunState,
  cause: string
): Promise<WorkflowRunReceipt> {
  const { failures } = await restoreEntries(state.journal.values())
  const base = {
    runId: run.runId,
    workflowId: run.workflowId,
    startedAt: run.startedAt,
    applied: 0,
    irreversible: run.irreversible
  }
  if (failures.length === 0) {
    await discardRunJournal(state.journalFile)
    return {
      ...base,
      paths: [],
      rolledBack: { reason: `${cause}. The run was rolled back; your vault is unchanged.` }
    }
  }
  // The paths still differing from their pre-run bytes are exactly the ones the
  // rollback could not reach, which is what `paths` promises to list.
  const unrestored = failures.map((failure) => failure.path)
  let reason =
    `${cause}. ROLLBACK INCOMPLETE: ${failures.length} of ${state.journal.size} ` +
    `files could not be restored (${describeFailures(failures)}). ` +
    `Undo run ${run.runId} to try again.`
  try {
    await writeLedger({
      ...ledgerBase(run, state),
      finishedAt: Date.now(),
      applied: 0,
      paths: unrestored,
      undone: false,
      rolledBack: { reason }
    })
    await discardRunJournal(state.journalFile)
  } catch (err) {
    // Nothing left to fall back on. The crash journal deliberately stays: it is
    // now the only copy of the pre-run bytes for files still holding this run's
    // writes, and the next run turns it into the undoable ledger this one could
    // not write.
    reason =
      `${reason} The run could not be recorded either (${messageOf(err)}), ` +
      `so undo is offered again only once ZenNotes can write to the vault's .zennotes folder.`
  }
  return { ...base, paths: unrestored, rolledBack: { reason } }
}

/**
 * Paths that are no longer what the run left there.
 *
 * Undo restores recorded bytes over whatever is in the file now, on purpose:
 * that is the only mechanism that is right by construction, and a note the run
 * changed is a note the user may well have kept working in. Those two facts
 * together mean an undo can take an edit with it that the run never made, so
 * the result names those files rather than reverting them in silence.
 */
async function driftedPathsOf(ledger: WorkflowRunLedger): Promise<string[]> {
  const drifted: string[] = []
  for (const entry of ledger.journal) {
    const after = ledger.hashes[entry.path]
    // A path the ledger holds no post-run hash for makes no claim about how it
    // was left (its write never landed), so it cannot have drifted.
    if (after === undefined) continue
    let live: string | null
    try {
      live = await readText(resolveVaultRel(entry.path))
    } catch {
      // A path undo is about to refuse anyway; the failure it reports there is
      // the useful one, not a drift report about a file it cannot read.
      continue
    }
    if ((live === null ? null : await hashText(live)) !== after) drifted.push(entry.path)
  }
  return drifted
}

export function undoWorkflowRun(runId: string): Promise<WorkflowUndoResult> {
  return withRunLock(() => undoWorkflowRunNow(runId))
}

async function undoWorkflowRunNow(runId: string): Promise<WorkflowUndoResult> {
  const rel = resolveLedgerRel(runId)
  let ledger: WorkflowRunLedger | null
  try {
    ledger = await readLedger(rel)
  } catch (err) {
    throw new Error(`Cannot read the record of run ${runId}: ${messageOf(err)}`)
  }
  // Undoing an unknown or already-undone run is an error, never a silent
  // success: "nothing happened" and "your vault is back" are different answers,
  // and only one of them is safe to show the user.
  if (!ledger) throw new Error(`Unknown workflow run: ${runId}`)
  if (ledger.undone) throw new Error(`Workflow run was already undone: ${runId}`)

  // Read before anything is restored, since afterwards every file matches the
  // journal and nothing about the drift is recoverable.
  const driftedPaths = await driftedPathsOf(ledger)

  const { restored, failures } = await restoreEntries(ledger.journal)
  // A partial restore throws WITHOUT marking the run undone. Restoring recorded
  // bytes is idempotent, so leaving the run undoable means the honest recovery
  // (try again) is also the correct one.
  if (failures.length > 0) {
    throw new Error(
      `Undo of run ${runId} is incomplete: ${failures.length} of ${ledger.journal.length} ` +
        `files could not be restored (${describeFailures(failures)}). ` +
        `The run is still undoable; try again.`
    )
  }

  await writeLedger({ ...ledger, undone: true, undoneAt: Date.now() })
  return { runId, restored, driftedPaths }
}

export async function listWorkflowRuns(): Promise<WorkflowRunSummary[]> {
  await withRunLock(() => recoverInterruptedRunsNow())
  let names: string[]
  try {
    names = await listDir(RUNS_REL_DIR)
  } catch {
    return []
  }
  const runs: WorkflowRunSummary[] = []
  for (const name of names) {
    if (!name.toLowerCase().endsWith('.json')) continue
    try {
      const ledger = await readLedger(`${RUNS_REL_DIR}/${name}`)
      if (!ledger) continue
      runs.push({
        runId: ledger.runId,
        workflowId: ledger.workflowId,
        startedAt: ledger.startedAt,
        applied: ledger.applied,
        paths: ledger.paths,
        // Nothing recorded means nothing to restore, so offering undo would be
        // a promise the journal cannot keep.
        undoable: !ledger.undone && ledger.journal.length > 0,
        // Only when true, so a normal run's summary is exactly the shape it has
        // always been on the wire.
        ...(ledger.interrupted ? { interrupted: true } : {})
      })
    } catch (err) {
      console.warn(`[workflows] skipping unreadable run ledger ${name}:`, err)
    }
  }
  runs.sort((a, b) => b.startedAt - a.startedAt || b.runId.localeCompare(a.runId))
  return runs
}

export function deleteWorkflowRuns(workflowId: string): Promise<number> {
  return withRunLock(() => deleteWorkflowRunsNow(workflowId))
}

/**
 * Remove every ledger a workflow's runs left behind, resolving to how many.
 *
 * Exists for the guided tutorial's leave-no-trace cleanup: the practice
 * workflow's ledgers hold copies of the practice notes' bytes, so "the vault
 * ends exactly as it started" includes them. An unreadable ledger is left
 * alone: it cannot be proven ours.
 */
async function deleteWorkflowRunsNow(workflowId: string): Promise<number> {
  let names: string[]
  try {
    names = await listDir(RUNS_REL_DIR)
  } catch {
    return 0
  }
  let removed = 0
  for (const name of names) {
    if (!name.toLowerCase().endsWith('.json')) continue
    try {
      const ledger = await readLedger(`${RUNS_REL_DIR}/${name}`)
      if (!ledger || ledger.workflowId !== workflowId) continue
      await deleteFile(`${RUNS_REL_DIR}/${name}`)
      // Its crash journal too, on the off chance one outlived the run: leaving
      // it would let recovery rebuild the very ledger just deleted, and this
      // exists so a workflow can leave no trace at all.
      await deleteFile(journalRelFor(ledger.runId))
      removed += 1
    } catch {
      /* unreadable: not provably this workflow's, so it stays */
    }
  }
  return removed
}
