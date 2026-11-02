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
 *
 * A note's comments, creation date and unreviewed assistant edits live in
 * `.zennotes`, keyed by the note's path (src-tauri/src/vault/sidecars.rs), so
 * a path op that moved only the Markdown would detach the discussion, lose the
 * date, and leave both behind to block or pollute the next note given the old
 * name (upstream #839). Path ops carry them under the same rules: each is
 * journalled as `{ note, sidecar, before }` before it moves, undo restores
 * those bytes, and the sidecar file is derived from the note's path at restore
 * time rather than stored, so a ledger can never name an arbitrary file
 * inside `.zennotes`.
 *
 * A symlinked note is the one path its bytes do not describe: a move renames
 * the LINK, so the file it points at, which may live outside the vault, never
 * moves at all. The journal records the link's text beside its bytes, and undo
 * moves that same link back from wherever the run left it, found by its text
 * and never made from what a ledger says. See `restoreEntries`.
 */

import { invoke } from './ipc'
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
  journal: NoteJournalEntry[]
  /** Content hash of what the run LEFT at each path (null where it removed the
   *  file), so undo can tell a file it wrote from one that has since changed. */
  hashes: Record<string, string | null>
  /** The sidecars the run moved with its notes. Beside `journal` rather than in
   *  it, and absent when there were none, because every other reader of a
   *  ledger (an older ZenNotes, the Go server) resolves each journal entry as a
   *  note path and would refuse a `.zennotes` one, leaving the run's undo
   *  failing forever. Kept apart, they skip the sidecars and still undo notes. */
  sidecars?: LedgerSidecarEntry[]
  undone: boolean
  undoneAt?: number
  rolledBack?: { reason: string }
  interrupted?: { reason: string }
}

/** A note's journal entry. `link` is the text of the symlink the path was
 *  before the run, when it was one: its bytes cannot say that, and a move
 *  renames the link itself, so undo needs the text to find that link again
 *  (see `restoreEntries`). Absent in ledgers from before it was recorded,
 *  which still undo, just without putting links back. */
interface NoteJournalEntry extends WorkflowJournalEntry {
  link?: string
}

/** The files `.zennotes` keeps for a note: its comments, its creation date,
 *  and an assistant's edits waiting for review. */
type SidecarKind = 'comments' | 'metadata' | 'agent-edits'

const SIDECAR_KINDS: readonly SidecarKind[] = ['comments', 'metadata', 'agent-edits']

/** How a failure names a sidecar, since the user knows it by what it holds. */
const SIDECAR_LABELS: Record<SidecarKind, string> = {
  comments: 'comments',
  metadata: 'creation date',
  'agent-edits': 'assistant edits waiting for review'
}

/** A sidecar's pre-run bytes: `before: null` means the note had none. It names
 *  the NOTE, as the run spelled it, and never the sidecar's own path. On a
 *  crash journal line the missing `path` is deliberate: an older ZenNotes
 *  takes every line with a `path` for a note's entry, and would fail the whole
 *  undo on one inside `.zennotes`. Without it those lines are skipped, and the
 *  notes still undo. */
interface SidecarJournalEntry {
  note: string
  sidecar: SidecarKind
  before: string | null
}

/** As a finished ledger keeps it: plus the hash of what the run left there
 *  (null where it moved the sidecar away), which undo's drift report
 *  compares. Absent where the run cannot know, as in a ledger rebuilt from a
 *  crash journal, the same way `hashes` is empty there. */
interface LedgerSidecarEntry extends SidecarJournalEntry {
  after?: string | null
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
/** Whether anything at all sits at a path, a link that points at nothing
 *  included. `exists` follows links, so it calls such a link free, and a
 *  rename onto it replaces the link without a trace. */
const pathTaken = (relPath: string): Promise<boolean> => invoke<boolean>('db_path_taken', { relPath })
/** The text of the symlink at a path, or undefined when it is not one. */
const linkTextOf = async (relPath: string): Promise<string | undefined> =>
  (await invoke<string | null>('db_link_text', { relPath })) ?? undefined
/** Give a symlink a new spelling of the same target (Rust checks it is one). */
const respellLink = (relPath: string, text: string): Promise<void> =>
  invoke('db_respell_link', { relPath, text })
/** Rename the way the app moves a note: a moved link keeps pointing at its
 *  file (`rebaseMovedLink`). */
const renameEntry = (oldRel: string, newRel: string): Promise<void> =>
  invoke('db_rename_entry', { oldRel, newRel })
/** Remove what sits at a path as itself, never through a link, and prune the
 *  folders that leaves empty below the top-level ones. */
const removeEntry = (relPath: string): Promise<void> => invoke('db_remove_entry', { relPath })
/** Prune empty folders upward inside the sidecar tree `relDir` belongs to. */
const pruneSidecarDirs = (relDir: string): Promise<void> =>
  invoke('db_prune_sidecar_dirs', { relDir })

/**
 * Where a note's sidecar lives, derived from the note's path every time.
 *
 * The note's path is checked by `resolveVaultRel` first, exactly as a journal
 * entry is, because at undo time it came off disk. Deriving the rest (in Rust,
 * which also refuses a symlink anywhere on a metadata path) is what keeps the
 * `.zennotes` refusal meaningful: a ledger can only ever aim a restore at some
 * note's own comments, creation date or edit record, never at a workflow, a
 * template or another run's record.
 */
function sidecarRelOf(note: string, sidecar: SidecarKind): Promise<string> {
  return invoke<string>('db_sidecar_rel', { noteRel: resolveVaultRel(note), kind: sidecar })
}

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
  // "Taken" includes a symlink that points at nothing: the rename would
  // replace it, and nothing could put it back.
  for (let n = 2; n < 1000; n += 1) {
    if (!(await pathTaken(candidate))) return candidate
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

/** Record journal entries on disk, opening the file on first use. Several
 *  entries share one append and one fsync: a process killed inside that write
 *  leaves a prefix of the lines, the tail of which the reader skips as a
 *  fragment, and the writes those lines protect have not begun. */
async function appendJournalEntries(
  file: RunJournalFile,
  entries: ReadonlyArray<NoteJournalEntry | SidecarJournalEntry>
): Promise<void> {
  if (entries.length === 0) return
  if (!file.started) {
    await appendText(file.rel, `${file.header}\n`)
    file.started = true
  }
  await appendText(file.rel, `${entries.map((entry) => JSON.stringify(entry)).join('\n')}\n`)
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
  journal: NoteJournalEntry[]
  sidecars: SidecarJournalEntry[]
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
  const journal: NoteJournalEntry[] = []
  const sidecars: SidecarJournalEntry[] = []
  for (const line of lines.slice(1)) {
    let parsed: unknown
    try {
      parsed = JSON.parse(line)
    } catch {
      continue // the truncated tail of a killed write
    }
    const sidecar = parseSidecarEntry(parsed)
    if (sidecar) {
      // Never with `after`: a crash journal cannot know how the run left things.
      sidecars.push({ note: sidecar.note, sidecar: sidecar.sidecar, before: sidecar.before })
      continue
    }
    const entry = parseNoteEntry(parsed)
    if (entry) journal.push(entry)
  }
  return {
    workflowId: typeof header.workflowId === 'string' ? header.workflowId : 'unknown',
    startedAt: typeof header.startedAt === 'number' ? header.startedAt : 0,
    irreversible: typeof header.irreversible === 'number' ? header.irreversible : 0,
    ops: Array.isArray(header.ops)
      ? header.ops.map(parseWorkflowOp).filter((op): op is WorkflowOp => op !== null)
      : [],
    journal,
    sidecars
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
  journal: Map<string, NoteJournalEntry>
  journalFile: RunJournalFile
  /** Journal key to the path as the run named it and the hash of what it left
   *  there (null where it removed the file). */
  written: Map<string, { path: string; hash: string | null }>
  /** The sidecars path ops carried, keyed by `sidecarKey`, first touch wins
   *  for the reason `journal` does: a note moved twice in one run leaves
   *  comments at the middle path only in between, and undo must remove them
   *  there rather than restore what the first move put down. */
  sidecars: Map<string, SidecarJournalEntry>
  /** Sidecar key to the hash of what the run left there (null: moved away). */
  sidecarsWritten: Map<string, string | null>
  /** Destination the plan promised to where the file actually landed, for the
   *  one case where they differ: `uniqueRel` had to suffix around a collision. */
  redirects: Map<string, string>
}

/** Whether this host's disks fold case: macOS and Windows by default. */
const CASE_INSENSITIVE_FS = (() => {
  const ua = (typeof navigator !== 'undefined' && (navigator.userAgent || navigator.platform)) || ''
  return /Mac|iPhone|iPad|iPod|Win/i.test(ua)
})()

/**
 * How the journal recognises two paths as the same file. On a case-insensitive
 * filesystem `Inbox/A.md` and `inbox/a.md` ARE one file, and journalling them
 * separately would break first-touch-wins: the second entry would record what
 * the first op had just written, and undo would restore that half-applied state
 * as if it were the original. Folding the key (never the stored path) keeps one
 * file to one entry. Linux is left alone, where those really are two files.
 */
function journalKey(rel: string): string {
  return CASE_INSENSITIVE_FS ? rel.toLowerCase() : rel
}

/** The entry, plus the text of the symlink its path is, when it is one. Read
 *  at the first touch, like the bytes, so it describes the path before the run. */
async function withLinkText(entry: WorkflowJournalEntry): Promise<NoteJournalEntry> {
  const link = await linkTextOf(resolveVaultRel(entry.path))
  return link === undefined ? entry : { ...entry, link }
}

/** One key per note and sidecar, folded like the note's own journal key. */
function sidecarKey(note: string, sidecar: SidecarKind): string {
  return `${sidecar}:${journalKey(note)}`
}

/**
 * Record the pre-run state of everything one op is about to change, on disk
 * before it is recorded in memory: the note (both ends of it, for a path op)
 * and each sidecar the op touches, first touch winning throughout.
 *
 * The order is the durability guarantee: every caller awaits this before the
 * write it describes, so a process killed at any point leaves a journal that
 * covers at least every file it had begun to change. It is ONE append and ONE
 * fsync for all of them, since the op writes none of these files until all
 * of them are durable either way.
 */
async function journalOp(
  state: RunState,
  notes: WorkflowJournalEntry[],
  sidecars: SidecarJournalEntry[]
): Promise<void> {
  const newNotes = new Map<string, NoteJournalEntry>()
  for (const entry of notes) {
    const key = journalKey(entry.path)
    if (!state.journal.has(key) && !newNotes.has(key)) newNotes.set(key, await withLinkText(entry))
  }
  const newSidecars = new Map<string, SidecarJournalEntry>()
  for (const entry of sidecars) {
    const key = sidecarKey(entry.note, entry.sidecar)
    if (!state.sidecars.has(key) && !newSidecars.has(key)) newSidecars.set(key, entry)
  }
  await appendJournalEntries(state.journalFile, [...newNotes.values(), ...newSidecars.values()])
  for (const [key, entry] of newNotes) state.journal.set(key, entry)
  for (const [key, entry] of newSidecars) state.sidecars.set(key, entry)
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
  const metadata = await sidecarRelOf(rel, 'metadata')
  // Creating a note where a date file waits with no note beside it: that date
  // belongs to nobody (#839), and `createNote` discards it too. Journalled,
  // so undo puts it back; the new note's date is its own birth time.
  const leftover = live === null ? await readText(metadata) : null
  if (live !== null) await keepCreationDate(rel)
  await journalOp(
    state,
    [{ path: rel, before: live }],
    leftover === null ? [] : [{ note: rel, sidecar: 'metadata', before: leftover }]
  )
  if (leftover !== null) {
    await deleteFile(metadata)
    state.sidecarsWritten.set(sidecarKey(rel, 'metadata'), null)
  }
  await writeThroughLinks(rel, next)
  recordWritten(state, rel, await hashText(next))
}

/**
 * Write a note's creation date down before its file is replaced.
 *
 * A note ZenNotes has never saved has no date file: its date is the file's own
 * birth time, and an atomic write (temp file plus rename) replaces that file
 * with one born now. Saving from the editor writes the date down first, and so
 * must every write here, and every move: the rename keeps the file, but undo
 * writes it back as a new one. A date file already there is left alone, valid
 * or not, so a corrupt one cannot fail a run the way it fails a save. The one
 * write that is not journalled first, safely: it records a date the note
 * already had, where the app already looks for it. Rust skips a temporary
 * folder session, which never gets app state written into it.
 */
async function keepCreationDate(rel: string): Promise<void> {
  if ((await readText(await sidecarRelOf(rel, 'metadata'))) !== null) return
  await invoke('db_prepare_note_creation', { relPath: rel })
}

/** A sidecar a path op is about to carry. */
interface SidecarMove {
  kind: SidecarKind
  fromRel: string
  toRel: string
  /** The note's own, or null when it has none. */
  fromBytes: string | null
  /** Whatever already sits at the destination's name. With no note there, it
   *  belongs to nobody. */
  toBytes: string | null
}

/** The words a move shows when an earlier note's comments sit on the
 *  destination name (`leftoverCommentsMessage` in sidecars.rs). */
function leftoverCommentsMessage(noteRel: string, commentsRel: string): string {
  const name = stripNoteExtension(relBasename(noteRel))
  return `Comments from an earlier note named “${name}” are still in ${commentsRel}. Move or delete that file to use this name.`
}

/**
 * The sidecars a path op carries, read before anything is journalled.
 *
 * `to` is free (`uniqueRel`), so anything already at its sidecar paths was left
 * by a note moved or deleted without them. The policy is the one every other
 * writer follows (`relocate_note` in sidecars.rs): a leftover creation date is
 * replaced, or removed, since it would stamp another note's date on this one;
 * leftover comments refuse, because taking over another note's discussion and
 * deleting it are both wrong. The refusal fails the run, which rolls it back,
 * and the message names the file to move aside.
 */
async function sidecarsToCarry(from: string, to: string): Promise<SidecarMove[]> {
  const moves: SidecarMove[] = []
  // Refused before anything is written down, so a refusal leaves no trace.
  const toComments = await sidecarRelOf(to, 'comments')
  if ((await readText(toComments)) !== null) {
    throw new Error(leftoverCommentsMessage(to, toComments))
  }
  // The rename keeps the file's birth time, but undo cannot: it writes the
  // note back as a new file. Written down now, the date travels and comes back
  // like any other sidecar.
  await keepCreationDate(from)
  for (const kind of SIDECAR_KINDS) {
    const fromRel = await sidecarRelOf(from, kind)
    const toRel = await sidecarRelOf(to, kind)
    const toBytes = await readText(toRel)
    const fromBytes = await readText(fromRel)
    if (fromBytes === null && toBytes === null) continue
    moves.push({ kind, fromRel, toRel, fromBytes, toBytes })
  }
  return moves
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
  // Before the first journal line, so an op refused over its sidecars leaves
  // nothing of itself to take back.
  const sidecars = await sidecarsToCarry(from, to)
  await journalOp(
    state,
    [
      { path: from, before: live },
      // `uniqueRel` just said this path is free, but it is read rather than
      // assumed null: another process can create a file inside that window, and
      // journalling it as "did not exist" would turn undo into a delete of
      // somebody's note.
      { path: to, before: await readText(to) }
    ],
    sidecars.flatMap((sidecar) => [
      ...(sidecar.fromBytes === null
        ? []
        : [{ note: from, sidecar: sidecar.kind, before: sidecar.fromBytes }]),
      { note: to, sidecar: sidecar.kind, before: sidecar.toBytes }
    ])
  )
  // A symlinked note keeps pointing where it did, as the app's own move keeps it.
  await renameEntry(from, to)
  recordWritten(state, from, null)
  recordWritten(state, to, await hashText(live))
  for (const sidecar of sidecars) {
    if (sidecar.fromBytes === null) {
      // Only a leftover date was waiting there, and this note has none of its
      // own to put in its place.
      await deleteFile(sidecar.toRel)
      state.sidecarsWritten.set(sidecarKey(to, sidecar.kind), null)
      continue
    }
    // Replaces a leftover date, when one was waiting there.
    await renameEntry(sidecar.fromRel, sidecar.toRel)
    state.sidecarsWritten.set(sidecarKey(from, sidecar.kind), null)
    state.sidecarsWritten.set(sidecarKey(to, sidecar.kind), await hashText(sidecar.fromBytes))
  }
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
 *
 * A symlinked note needs more than its bytes. A move renames the link itself,
 * so the destination holds the link while the file it points at never moved.
 * Putting the vault back means moving that same link back, and a path the run
 * filled is emptied by removing what sits there as itself, never through a
 * link: the file a link points at is not the run's to delete. The one
 * exception is a link that pointed at nothing before the run, whose target the
 * run created by writing through it; its entry records the link's text, and
 * only while that same link is there is its target removed.
 *
 * A link is only ever moved back, never made: it is found by its recorded text
 * among the paths the run filled. Creating one from what a ledger says would
 * let an edited or synced ledger plant a link to anywhere and then write
 * through it. With no link to move back (a ledger from before the text was
 * recorded, or a link removed since), the bytes come back as a plain file.
 */
async function restoreEntries(
  entries: Iterable<NoteJournalEntry>
): Promise<{ restored: number; failures: RestoreFailure[] }> {
  const failures: RestoreFailure[] = []
  let restored = 0
  const list = [...entries]
  // Where the run left the links it moved, by the file each names: a path the
  // run found empty that now holds a link it did not have before. By the file
  // rather than the text, because a relative text was re-based when the link
  // moved, and is again on the way back.
  const movedLinks = new Map<string, string[]>()
  for (const entry of list) {
    if (entry.before !== null) continue
    try {
      const safe = resolveVaultRel(entry.path)
      const text = await linkTextOf(safe)
      if (text === undefined || text === entry.link) continue
      const names = linkNames(safe, text)
      const same = movedLinks.get(names)
      if (same) same.push(safe)
      else movedLinks.set(names, [safe])
    } catch {
      // The loop below meets the same entry and reports it.
    }
  }
  for (const { path: rel, before, link } of list) {
    try {
      // Re-validated on the way back out. At rollback time these paths came
      // from this run, but undo replays the same code over a file read off
      // disk, and that file must never be able to point a write anywhere.
      const safe = resolveVaultRel(rel)
      if (link !== undefined && !(await pathTaken(safe))) {
        const names = linkNames(safe, link)
        const moved = movedLinks.get(names)?.pop()
        const text = moved === undefined ? undefined : await linkTextOf(moved)
        if (moved !== undefined && text !== undefined && linkNames(moved, text) === names) {
          await renameEntry(moved, safe)
          // The recorded text, when it names the very file the link now names
          // from here: the same reach, spelled the way it was.
          if ((await linkTextOf(safe)) !== link) await respellLink(safe, link)
        }
      }
      if (before === null) {
        // Empty before the run, so whatever sits there now goes, as itself.
        // Behind the link the path had before the run, pointing at nothing
        // then, it is the file the run created by writing through that link.
        const text = await linkTextOf(safe)
        if (text !== undefined && text === link) await removeThroughLinks(safe)
        else if (await pathTaken(safe)) await removeEntry(safe)
        restored += 1
        continue
      }
      // A path is journalled BEFORE its write is attempted, so a run that
      // failed mid-way has journalled paths it never actually changed.
      // Restoring those is pointless at best: whatever failed the write fails
      // the restore too, and the rollback then reports ROLLBACK INCOMPLETE over
      // a file that was never touched.
      if ((await readText(safe)) === before) {
        restored += 1
        continue
      }
      await writeThroughLinks(safe, before)
      restored += 1
    } catch (err) {
      failures.push({ path: rel, message: messageOf(err) })
    }
  }
  return { restored, failures }
}

/** The vault path a link's text names, from the link's own folder. Textual,
 *  like `path.resolve(dirname, text)`: `.` and `..` segments are collapsed,
 *  so two spellings of one target compare equal. An absolute text names
 *  itself; a target above the vault keeps its leading `..` segments. */
function linkNames(linkRel: string, text: string): string {
  if (text.startsWith('/')) return text
  const out: string[] = []
  for (const segment of joinRel(relDirname(linkRel), text).replace(/\\/g, '/').split('/')) {
    if (!segment || segment === '.') continue
    if (segment === '..' && out.length > 0 && out[out.length - 1] !== '..') out.pop()
    else out.push(segment)
  }
  return out.join('/')
}

/**
 * Put every journalled sidecar back, the way `restoreEntries` does notes. A
 * failure is reported under its NOTE's path, which is the name the user knows
 * and the one a receipt lists; the message says which sidecar it was.
 */
async function restoreSidecars(entries: Iterable<SidecarJournalEntry>): Promise<RestoreFailure[]> {
  const failures: RestoreFailure[] = []
  const list = [...entries]
  // Files this run put where none had been, by what they hold now. When one
  // still holds exactly the bytes another entry must get back (a sidecar the
  // run moved and nobody touched since), renaming it there restores those
  // bytes and removes it from where the run put it, which is what writing the
  // one and deleting the other would do, minus a synced write per sidecar.
  // Only ever after comparing the bytes, so this is the same restore.
  const moved = new Map<string, string[]>()
  for (const entry of list) {
    if (entry.before !== null) continue
    try {
      const rel = await sidecarRelOf(entry.note, entry.sidecar)
      const live = await readText(rel)
      if (live === null) continue
      const key = `${entry.sidecar}\n${live}`
      const same = moved.get(key)
      if (same) same.push(rel)
      else moved.set(key, [rel])
    } catch {
      // The loop below meets the same entry and reports it.
    }
  }
  for (const entry of list) {
    try {
      const rel = await sidecarRelOf(entry.note, entry.sidecar)
      const live = await readText(rel)
      // Unchanged since the run journalled it, for the reason given in
      // `restoreEntries`: a failed run journals sidecars it never moved.
      if (live === entry.before) continue
      if (entry.before === null) {
        await deleteFile(rel)
        await pruneSidecarDirs(relDirname(rel))
        continue
      }
      const source =
        live === null ? moved.get(`${entry.sidecar}\n${entry.before}`)?.pop() : undefined
      // Read again right before the rename, so nothing written in between
      // rides along with it.
      if (source !== undefined && (await readText(source)) === entry.before) {
        try {
          await renameEntry(source, rel)
          await pruneSidecarDirs(relDirname(source))
          continue
        } catch {
          // Writing the bytes is the restore; the rename was only quicker.
        }
      }
      await writeText(rel, entry.before)
    } catch (err) {
      failures.push({
        path: entry.note,
        message: `its ${SIDECAR_LABELS[entry.sidecar]}: ${messageOf(err)}`
      })
    }
  }
  return failures
}

/** A run's notes and their sidecars, put back together. `restored` counts
 *  notes only: it is what the undo toast reports, and a note whose comments
 *  came back with it is still one note. */
async function restoreRun(
  journal: Iterable<NoteJournalEntry>,
  sidecars: Iterable<SidecarJournalEntry>
): Promise<{ restored: number; failures: RestoreFailure[] }> {
  const notes = await restoreEntries(journal)
  const sidecarFailures = await restoreSidecars(sidecars)
  return { restored: notes.restored, failures: [...notes.failures, ...sidecarFailures] }
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

/** One note entry, from a ledger or a crash journal line, or null. */
function parseNoteEntry(value: unknown): NoteJournalEntry | null {
  if (!isRecord(value)) return null
  const { path: entryPath, before, link } = value
  if (typeof entryPath !== 'string') return null
  if (typeof before !== 'string' && before !== null) return null
  return typeof link === 'string' ? { path: entryPath, before, link } : { path: entryPath, before }
}

function parseJournal(value: unknown): NoteJournalEntry[] {
  if (!Array.isArray(value)) return []
  return value
    .map((item) => parseNoteEntry(item))
    .filter((entry): entry is NoteJournalEntry => entry !== null)
}

/** One sidecar entry, from a ledger or a crash journal line, or null. A kind
 *  this version does not know is dropped rather than guessed at. */
function parseSidecarEntry(value: unknown): LedgerSidecarEntry | null {
  if (!isRecord(value)) return null
  const { note, sidecar, before, after } = value
  if (typeof note !== 'string') return null
  if (sidecar !== 'comments' && sidecar !== 'metadata' && sidecar !== 'agent-edits') return null
  if (typeof before !== 'string' && before !== null) return null
  const entry: LedgerSidecarEntry = { note, sidecar, before }
  if (typeof after === 'string' || after === null) entry.after = after
  return entry
}

function parseSidecars(value: unknown): LedgerSidecarEntry[] {
  if (!Array.isArray(value)) return []
  return value
    .map((item) => parseSidecarEntry(item))
    .filter((entry): entry is LedgerSidecarEntry => entry !== null)
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
  const sidecars = parseSidecars(parsed.sidecars)
  if (sidecars.length > 0) ledger.sidecars = sidecars
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
        // Without `after`, for the reason `hashes` is empty.
        ...(parsed.sidecars.length > 0 ? { sidecars: parsed.sidecars } : {}),
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
    sidecars: new Map(),
    sidecarsWritten: new Map(),
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
    ),
    ...(state.sidecars.size > 0 ? { sidecars: sidecarEntries(state) } : {})
  }
}

/** The journalled sidecars, each with what the run left there when it got as
 *  far as moving it (a rolled-back run may not have). */
function sidecarEntries(state: RunState): LedgerSidecarEntry[] {
  return [...state.sidecars.entries()].map(([key, entry]) => {
    const after = state.sidecarsWritten.get(key)
    return after === undefined ? { ...entry } : { ...entry, after }
  })
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
  const { failures } = await restoreRun(state.journal.values(), state.sidecars.values())
  const base = {
    runId: run.runId,
    workflowId: run.workflowId,
    startedAt: run.startedAt,
    applied: 0,
    irreversible: run.irreversible
  }
  // A cause can be a whole sentence already (the create-note refusal, the
  // leftover-comments one shared with the rest of the app), and the reason
  // below adds its own full stop.
  const stated = cause.replace(/\.\s*$/, '')
  if (failures.length === 0) {
    await discardRunJournal(state.journalFile)
    return {
      ...base,
      paths: [],
      rolledBack: { reason: `${stated}. The run was rolled back; your vault is unchanged.` }
    }
  }
  // The paths still differing from their pre-run bytes are exactly the ones the
  // rollback could not reach, which is what `paths` promises to list. Once
  // each: a note and its comments can both have failed.
  const unrestored = [...new Set(failures.map((failure) => failure.path))]
  let reason =
    `${stated}. ROLLBACK INCOMPLETE: ${failures.length} of ${state.journal.size + state.sidecars.size} ` +
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
  // A comment added to a moved note since the run is an edit to that note as
  // far as its author is concerned, and undo takes it away all the same, so
  // it is reported under the note's name.
  for (const entry of ledger.sidecars ?? []) {
    if (entry.after === undefined || drifted.includes(entry.note)) continue
    let live: string | null
    try {
      live = await readText(await sidecarRelOf(entry.note, entry.sidecar))
    } catch {
      continue
    }
    if ((live === null ? null : await hashText(live)) !== entry.after) drifted.push(entry.note)
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

  const sidecars = ledger.sidecars ?? []
  const { restored, failures } = await restoreRun(ledger.journal, sidecars)
  // A partial restore throws WITHOUT marking the run undone. Restoring recorded
  // bytes is idempotent, so leaving the run undoable means the honest recovery
  // (try again) is also the correct one.
  if (failures.length > 0) {
    throw new Error(
      `Undo of run ${runId} is incomplete: ${failures.length} of ${ledger.journal.length + sidecars.length} ` +
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
