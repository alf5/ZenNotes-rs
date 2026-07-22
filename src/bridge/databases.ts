/**
 * CSV databases — the webview half over the Rust `db_*` file commands.
 *
 * Adaptation of upstream `apps/desktop/src/main/databases.ts` (which runs in
 * the Electron main process): the pure CSV/schema logic already ships in the
 * vendored shared-domain packages and runs here unchanged — parse/serialize,
 * schema inference, default views — so on-disk format and inference behavior
 * stay byte-identical to upstream. Rust only reads/writes/renames files
 * inside the vault (atomic writes for data.csv/schema.json).
 *
 * A database is a self-contained `<Name>.base/` folder holding `data.csv`,
 * `schema.json`, and record-page notes; legacy loose `.csv` +
 * `<name>.csv.base.json` sidecars are still read (and migrated on open).
 */

import { invoke } from '@tauri-apps/api/core'
import {
  buildDefaultViews,
  inferFields,
  parseCsv,
  parseRows,
  serializeRows
} from '@zennotes/shared-domain/database-csv'
import {
  csvPathForFormDir,
  DATABASE_SIDECAR_SUFFIX,
  databaseSchemaPathFor,
  FORM_DIR_SUFFIX,
  formDirFromCsvPath,
  formTitleFromCsvPath,
  type DatabaseDoc,
  type DatabaseSidecar,
  type DbField,
  type DbRow,
  type DbView
} from '@zennotes/shared-domain/databases'
import type { NoteFolder } from '@zennotes/bridge-contract/ipc'

const SCHEMA_SAMPLE_ROWS = 50

const readText = (relPath: string): Promise<string | null> => invoke('db_read_text', { relPath })
const writeText = (relPath: string, text: string): Promise<void> =>
  invoke('db_write_text', { relPath, text })
const exists = (relPath: string): Promise<boolean> => invoke('db_exists', { relPath })

function randomUUID(): string {
  return typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `${Date.now().toString(16)}-${Math.random().toString(16).slice(2)}`
}

function toPosix(p: string): string {
  return p.replace(/\\/g, '/')
}

/** Database title: its `.base` folder name (legacy: the `.csv` basename). */
function titleFromPath(rel: string): string {
  const posix = toPosix(rel)
  if (formDirFromCsvPath(posix)) return formTitleFromCsvPath(posix)
  const base = posix.split('/').filter(Boolean).pop() ?? rel
  return base.replace(/\.csv$/i, '')
}

function sidecarPathFor(csvRel: string): string {
  return databaseSchemaPathFor(toPosix(csvRel)) ?? `${toPosix(csvRel)}${DATABASE_SIDECAR_SUFFIX}`
}

// Record-page paths are stored RELATIVE to the database folder (e.g. `X.md`)
// so a folder rename/move never rewrites them; on read they're resolved to
// full vault-relative paths. Legacy loose databases pass through unchanged.
function pagesToFull(csvRel: string, pages: Record<string, string>): Record<string, string> {
  const formDir = formDirFromCsvPath(toPosix(csvRel))
  if (!formDir) return pages
  const prefix = `${formDir}/`
  return Object.fromEntries(
    Object.entries(pages).map(([id, p]) => [id, p.startsWith(prefix) ? p : `${prefix}${p}`])
  )
}

function pagesToRelative(csvRel: string, pages: Record<string, string>): Record<string, string> {
  const formDir = formDirFromCsvPath(toPosix(csvRel))
  if (!formDir) return pages
  const prefix = `${formDir}/`
  return Object.fromEntries(
    Object.entries(pages).map(([id, p]) => [id, p.startsWith(prefix) ? p.slice(prefix.length) : p])
  )
}

/** Defensive parse of a sidecar JSON; returns null when missing or unusable.
 *  Verbatim upstream databases.ts:89. */
function normalizeSidecar(raw: unknown): DatabaseSidecar | null {
  if (!raw || typeof raw !== 'object') return null
  const obj = raw as Record<string, unknown>
  const fields = Array.isArray(obj.fields) ? (obj.fields as DbField[]) : null
  if (!fields || fields.length === 0) return null
  if (!fields.every((f) => f && typeof f.id === 'string' && typeof f.name === 'string')) return null
  const fieldIds = new Set(fields.map((f) => f.id))
  const idFieldId =
    typeof obj.idFieldId === 'string' && fieldIds.has(obj.idFieldId) ? obj.idFieldId : fields[0].id
  let views = Array.isArray(obj.views) ? (obj.views as DbView[]) : []
  views = views.filter(
    (v) => v && typeof v.id === 'string' && (v.type === 'table' || v.type === 'board')
  )
  if (views.length === 0) views = buildDefaultViews(fields).views
  const activeViewId =
    typeof obj.activeViewId === 'string' && views.some((v) => v.id === obj.activeViewId)
      ? obj.activeViewId
      : views[0].id
  const pages =
    obj.pages && typeof obj.pages === 'object'
      ? (Object.fromEntries(
          Object.entries(obj.pages as Record<string, unknown>).filter(
            ([, v]) => typeof v === 'string'
          )
        ) as Record<string, string>)
      : undefined
  return { version: 1, idFieldId, fields, views, activeViewId, ...(pages ? { pages } : {}) }
}

async function readSidecar(csvRel: string): Promise<DatabaseSidecar | null> {
  const raw = await readText(sidecarPathFor(csvRel))
  if (raw === null) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return null
  }
  const sidecar = normalizeSidecar(parsed)
  if (sidecar?.pages) sidecar.pages = pagesToFull(toPosix(csvRel), sidecar.pages)
  return sidecar
}

function hydrate(
  rel: string,
  sidecar: DatabaseSidecar,
  rows: DbRow[],
  pageHasContent?: Record<string, boolean>
): DatabaseDoc {
  return {
    ...sidecar,
    path: toPosix(rel),
    title: titleFromPath(rel),
    rows,
    ...(pageHasContent ? { pageHasContent } : {})
  }
}

/** True if a note has body content beyond its frontmatter + a single title heading. */
function noteHasBody(text: string): boolean {
  let body = text
  const fm = /^---\r?\n[\s\S]*?\r?\n---\r?\n?/.exec(body)
  if (fm) body = body.slice(fm[0].length)
  body = body.replace(/^\s*#[^\n]*\r?\n?/, '') // drop a single leading heading
  return body.trim().length > 0
}

async function readPageContentFlags(
  pages?: Record<string, string>
): Promise<Record<string, boolean> | undefined> {
  if (!pages || Object.keys(pages).length === 0) return undefined
  const flags: Record<string, boolean> = {}
  await Promise.all(
    Object.entries(pages).map(async ([rowId, notePath]) => {
      const text = await readText(notePath).catch(() => null)
      if (text !== null) flags[rowId] = noteHasBody(text)
      /* missing note → leave unset (treated as empty) */
    })
  )
  return flags
}

async function persistSidecar(csvRel: string, sidecar: DatabaseSidecar): Promise<void> {
  // Store record-page paths relative to the database folder (so a folder
  // rename never rewrites them); a legacy loose database has no folder.
  const onDisk: DatabaseSidecar = sidecar.pages
    ? { ...sidecar, pages: pagesToRelative(toPosix(csvRel), sidecar.pages) }
    : sidecar
  await writeText(sidecarPathFor(csvRel), `${JSON.stringify(onDisk, null, 2)}\n`)
}

// ---------------------------------------------------------------------------
// Bridge surface
// ---------------------------------------------------------------------------

/**
 * Read a database; null when the CSV is missing (stale tab). If no sidecar
 * exists, infer the schema from the CSV and adopt it: write the sidecar and
 * re-materialize the CSV so every row gains a stable `id`.
 */
export async function openDatabase(relPath: string): Promise<DatabaseDoc | null> {
  const csvText = await readText(relPath)
  if (csvText === null) return null

  const existing = await readSidecar(relPath)
  if (existing) {
    const rows = parseRows(csvText, existing.fields, existing.idFieldId, randomUUID)
    const pageHasContent = await readPageContentFlags(existing.pages)
    return hydrate(relPath, existing, rows, pageHasContent)
  }

  // Adopt a plain CSV: infer + materialize.
  const grid = parseCsv(csvText)
  const headers = grid[0] ?? []
  const { fields, idFieldId } = inferFields(headers, grid.slice(1, 1 + SCHEMA_SAMPLE_ROWS), randomUUID)
  const { views, activeViewId } = buildDefaultViews(fields, randomUUID)
  const sidecar: DatabaseSidecar = { version: 1, idFieldId, fields, views, activeViewId }
  const rows = parseRows(csvText, fields, idFieldId, randomUUID)
  await persistSidecar(relPath, sidecar)
  await writeText(relPath, serializeRows(rows, fields)) // canonicalize + persist ids
  return hydrate(relPath, sidecar, rows)
}

/** Persist rows to the CSV (schema/header come from the sidecar). */
export async function writeDatabaseRows(relPath: string, rows: DbRow[]): Promise<DatabaseDoc> {
  const sidecar = await readSidecar(relPath)
  if (!sidecar) throw new Error(`Database sidecar missing: ${relPath}`)
  await writeText(relPath, serializeRows(rows, sidecar.fields))
  return hydrate(
    relPath,
    sidecar,
    rows.map((r) => ({ ...r }))
  )
}

/** Persist schema + views AND rewrite the CSV under the new header (the
 *  caller's in-memory rows are authoritative, keyed by stable field.id). */
export async function writeDatabaseSchema(
  relPath: string,
  sidecar: DatabaseSidecar,
  rows: DbRow[]
): Promise<DatabaseDoc> {
  const normalized = normalizeSidecar(sidecar)
  if (!normalized) throw new Error(`Invalid database schema: ${relPath}`)
  await persistSidecar(relPath, normalized)
  await writeText(relPath, serializeRows(rows, normalized.fields))
  return hydrate(
    relPath,
    normalized,
    rows.map((r) => ({ ...r }))
  )
}

/** Create a new empty database (`id` + `Name` fields) under folder/subpath. */
export async function createDatabase(
  folder: NoteFolder,
  subpath: string,
  title?: string
): Promise<DatabaseDoc> {
  const safeTitle = (title ?? 'Untitled Database').trim() || 'Untitled Database'
  const baseName = safeTitle.replace(/[\\/:*?"<>|]/g, '-')
  const topRel = await invoke<string>('db_folder_root_rel', { folder })
  const cleanSub = toPosix(subpath).replace(/^\/+|\/+$/g, '')
  const dirRel = [topRel, cleanSub].filter(Boolean).join('/')
  const makeFormDir = (name: string): string =>
    dirRel ? `${dirRel}/${name}${FORM_DIR_SUFFIX}` : `${name}${FORM_DIR_SUFFIX}`
  // Resolve a non-colliding `<Name>.base` folder under the directory.
  let formDirRel = makeFormDir(baseName)
  let n = 2
  while (await exists(csvPathForFormDir(formDirRel))) {
    formDirRel = makeFormDir(`${baseName} ${n++}`)
  }
  const rel = csvPathForFormDir(formDirRel)

  const idField: DbField = { id: randomUUID(), name: 'id', type: 'text', hidden: true }
  const nameField: DbField = { id: randomUUID(), name: 'Name', type: 'text' }
  const fields = [idField, nameField]
  const { views, activeViewId } = buildDefaultViews(fields)
  const sidecar: DatabaseSidecar = {
    version: 1,
    idFieldId: idField.id,
    fields,
    views,
    activeViewId
  }
  await invoke('db_mkdir', { relPath: formDirRel })
  await persistSidecar(rel, sidecar)
  await writeText(rel, serializeRows([], fields))
  return hydrate(rel, sidecar, [])
}

/** Rename a database's `.base` folder (non-colliding); returns the new
 *  data.csv path. Everything lives inside, so nothing else is rewritten. */
export async function renameDatabase(csvPath: string, newTitle: string): Promise<string> {
  const formDir = formDirFromCsvPath(toPosix(csvPath))
  if (!formDir) throw new Error(`Not a database folder: ${csvPath}`)
  const parentRel = formDir.includes('/') ? formDir.slice(0, formDir.lastIndexOf('/')) : ''
  const safeName = (newTitle.trim() || 'Untitled Database').replace(/[\\/:*?"<>|]/g, '-')
  const makeFormDir = (name: string): string =>
    parentRel ? `${parentRel}/${name}${FORM_DIR_SUFFIX}` : `${name}${FORM_DIR_SUFFIX}`
  let targetRel = makeFormDir(safeName)
  if (toPosix(targetRel) === toPosix(formDir)) return csvPath
  let n = 2
  while (await exists(csvPathForFormDir(targetRel))) {
    targetRel = makeFormDir(`${safeName} ${n++}`)
  }
  await invoke('db_rename', { oldRel: formDir, newRel: targetRel })
  return csvPathForFormDir(targetRel)
}

/** Create a record-page note inside the database folder; returns its path. */
export function createRecordPage(csvPath: string, title: string, body: string): Promise<string> {
  const formDir = formDirFromCsvPath(toPosix(csvPath))
  if (!formDir) return Promise.reject(new Error(`Not a database folder: ${csvPath}`))
  return invoke('db_create_record_page', { formDirRel: formDir, title, body })
}
