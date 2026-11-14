/**
 * Drives one MCP server over stdio through scenario.json and returns what it
 * said, normalized so two servers can be compared: upstream's TypeScript
 * server (generate.ts records it as golden.json) and `zennotes-rs mcp`
 * (check.ts compares against it).
 *
 * Both servers run on a fresh copy of ./vault with every file's mtime pinned,
 * under a throwaway HOME / config dir, with ZENNOTES_VAULT naming the vault, so
 * nothing on this machine leaks in and nothing outside the copy is written.
 *
 * scenario.json's `steps` run on one server started with no flags. Each entry
 * of its `sessions` starts another server on another fresh copy, with its own
 * extra argv (the scope flags: `--allow-path`, `--read-only`, …), extra files
 * and symlinks written into the copy first, and its own steps. A session with
 * `expectExit` records how the server refused to start instead.
 */

import {
  cpSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  utimesSync,
  writeFileSync
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'

export const HERE = path.dirname(new URL(import.meta.url).pathname)
export const FIXTURE_VAULT = path.join(HERE, 'vault')
export const GOLDEN_FILE = path.join(HERE, 'golden.json')

/** 2026-01-01T00:00:00Z; fixture file i (sorted by path) is i minutes later. */
const FIXTURE_EPOCH_MS = 1767225600000

interface Step {
  tool: string
  args: Record<string, unknown>
  label?: string
  /** Sort an array result by these comma-separated fields. Upstream walks
   *  folders in readdir order, which differs between file systems, and lists
   *  tasks in whatever order its parallel reads finish. */
  sortBy?: string
  /** Sort the array at this field of the result instead of the result. */
  sortAt?: string
}

interface Session {
  label: string
  /** Appended to the server's command line. */
  args?: string[]
  /** Written into the vault copy before the server starts: text, or
   *  `{ base64 }` for bytes. Their mtimes are pinned too. */
  files?: Record<string, string | { base64: string }>
  /** Vault-relative link -> target, created in the copy. */
  symlinks?: Record<string, string>
  /** The server must refuse to start: record its exit code and message. */
  expectExit?: boolean
  steps: Step[]
}

export interface SessionRecord {
  label: string
  args: string[]
  /** What the server adds to its main-session instructions (the scope's
   *  `## Access` section). */
  instructionsSuffix: string
  /** The tools it lists, by name. */
  tools: string[]
  steps: StepRecord[]
  files: Record<string, unknown>
  exit?: { code: number | null; stderr: string[] }
}

export interface StepRecord {
  tool: string
  args: Record<string, unknown>
  isError: boolean
  result: unknown
}

export interface Transcript {
  instructions: string
  tools: unknown[]
  steps: StepRecord[]
  /** Every file in the vault after the last step, vault-relative. */
  files: Record<string, unknown>
  sessions: SessionRecord[]
}

function listFiles(dir: string, base = dir): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name)
    if (entry.isDirectory()) out.push(...listFiles(full, base))
    else out.push(path.relative(base, full).split(path.sep).join('/'))
  }
  return out.sort()
}

function prepare(session?: Session): { tmp: string; vault: string; env: Record<string, string> } {
  const tmp = mkdtempSync(path.join(os.tmpdir(), 'zn-mcp-golden-'))
  const vault = path.join(tmp, 'vault')
  cpSync(FIXTURE_VAULT, vault, { recursive: true })
  for (const [rel, body] of Object.entries(session?.files ?? {})) {
    const abs = path.join(vault, rel)
    mkdirSync(path.dirname(abs), { recursive: true })
    writeFileSync(abs, typeof body === 'string' ? body : Buffer.from(body.base64, 'base64'))
  }
  listFiles(vault).forEach((rel, i) => {
    const at = new Date(FIXTURE_EPOCH_MS + i * 60_000)
    utimesSync(path.join(vault, rel), at, at)
  })
  for (const [rel, target] of Object.entries(session?.symlinks ?? {})) {
    const abs = path.join(vault, rel)
    mkdirSync(path.dirname(abs), { recursive: true })
    symlinkSync(target, abs)
  }
  const home = path.join(tmp, 'home')
  const config = path.join(tmp, 'config')
  mkdirSync(home, { recursive: true })
  mkdirSync(config, { recursive: true })
  return {
    tmp,
    vault,
    env: {
      PATH: process.env.PATH ?? '',
      HOME: home,
      USERPROFILE: home,
      APPDATA: path.join(home, 'AppData', 'Roaming'),
      XDG_CONFIG_HOME: path.join(home, '.config'),
      XDG_DATA_HOME: path.join(home, '.local', 'share'),
      ZENNOTES_CONFIG_DIR: config,
      ZENNOTES_VAULT: vault,
      TZ: 'UTC',
      // Month and weekday names in a daily note's title follow the locale.
      LANG: 'en_US.UTF-8'
    }
  }
}

/* ---------- Normalization ------------------------------------------------ */

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const UUID_IN_TEXT_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi
/** `new Date().toISOString()`, as a deleted attachment's record holds it. */
const ISO_TIME_RE = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z/g

class Normalizer {
  private ids = new Map<string, string>()
  /** Today in the servers' time zone (UTC): a task finished today records it. */
  private readonly today = new Date().toISOString().slice(0, 10)
  constructor(private readonly root: string) {}

  value(v: unknown): unknown {
    if (typeof v === 'number') return v > 1e12 ? '<time>' : v
    if (typeof v === 'string') return this.string(v)
    if (Array.isArray(v)) return v.map((x) => this.value(x))
    if (v && typeof v === 'object') {
      const out: Record<string, unknown> = {}
      for (const [k, x] of Object.entries(v)) out[k] = this.value(x)
      return out
    }
    return v
  }

  private id(uuid: string): string {
    const key = uuid.toLowerCase()
    if (!this.ids.has(key)) this.ids.set(key, `<id${this.ids.size + 1}>`)
    return this.ids.get(key)!
  }

  string(s: string): string {
    if (UUID_RE.test(s)) return this.id(s)
    return s
      .split(this.root)
      .join('<ROOT>')
      .replace(UUID_IN_TEXT_RE, (u) => this.id(u))
      .replace(ISO_TIME_RE, '<isotime>')
      .split(this.today)
      .join('<today>')
  }
}

function parseMaybeJson(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    return text
  }
}

function at(value: unknown, dotted: string): unknown {
  let cur = value
  for (const key of dotted.split('.')) {
    cur = (cur as Record<string, unknown> | undefined)?.[key]
  }
  return cur
}

function resolveRefs(args: Record<string, unknown>, labelled: Map<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(args)) {
    if (v && typeof v === 'object' && '$ref' in (v as object)) {
      const ref = v as { $ref: string; at: string }
      out[k] = at(labelled.get(ref.$ref), ref.at)
    } else {
      out[k] = v
    }
  }
  return out
}

/* ---------- JSON-RPC over stdio ----------------------------------------- */

class StdioClient {
  private nextId = 1
  private pending = new Map<number, (msg: Record<string, unknown>) => void>()
  private buffer = ''

  constructor(private readonly proc: ReturnType<typeof Bun.spawn>) {
    void this.pump()
  }

  private async pump(): Promise<void> {
    const reader = (this.proc.stdout as ReadableStream<Uint8Array>).getReader()
    const decoder = new TextDecoder()
    for (;;) {
      const { value, done } = await reader.read()
      if (done) break
      this.buffer += decoder.decode(value, { stream: true })
      let nl: number
      while ((nl = this.buffer.indexOf('\n')) >= 0) {
        const line = this.buffer.slice(0, nl).trim()
        this.buffer = this.buffer.slice(nl + 1)
        if (!line) continue
        const msg = JSON.parse(line) as Record<string, unknown>
        const resolve = typeof msg.id === 'number' ? this.pending.get(msg.id) : undefined
        if (resolve) {
          this.pending.delete(msg.id as number)
          resolve(msg)
        }
      }
    }
    for (const resolve of this.pending.values()) resolve({ error: { message: 'server exited' } })
  }

  private send(msg: Record<string, unknown>): void {
    const stdin = this.proc.stdin as import('bun').FileSink
    stdin.write(JSON.stringify({ jsonrpc: '2.0', ...msg }) + '\n')
    stdin.flush()
  }

  request(method: string, params: unknown): Promise<Record<string, unknown>> {
    const id = this.nextId++
    const reply = new Promise<Record<string, unknown>>((resolve) => this.pending.set(id, resolve))
    this.send({ id, method, params })
    return reply
  }

  notify(method: string): void {
    this.send({ method })
  }

  async close(): Promise<void> {
    ;(this.proc.stdin as import('bun').FileSink).end()
    const timer = setTimeout(() => this.proc.kill(), 5000)
    await this.proc.exited
    clearTimeout(timer)
  }
}

interface Scenario {
  steps: Step[]
  sessions?: Session[]
}

/** A tool's answer as recorded: its text parsed as JSON when it is JSON, or,
 *  when it answers in more than one part or in more than text (read_asset),
 *  every part. */
function recordContent(content: Record<string, unknown>[]): unknown {
  if (content.length <= 1 && content.every((c) => c.type === 'text')) {
    return parseMaybeJson(content.map((c) => c.text as string).join(''))
  }
  return content.map((c) => {
    if (c.type === 'text') return { type: 'text', text: parseMaybeJson(c.text as string) }
    if (c.type === 'resource') {
      const r = c.resource as Record<string, unknown>
      return { type: 'resource', resource: JSON.parse(JSON.stringify({ uri: r.uri, mimeType: r.mimeType, blob: r.blob, text: r.text })) }
    }
    return { type: c.type, mimeType: c.mimeType, data: c.data }
  })
}

function snapshot(vault: string, norm: Normalizer): Record<string, unknown> {
  const files: Record<string, unknown> = {}
  for (const rel of listFiles(vault)) {
    const abs = path.join(vault, rel)
    if (lstatSync(abs).isSymbolicLink()) continue
    const bytes = readFileSync(abs)
    const text = bytes.toString('utf8')
    if (rel.endsWith('.comments.json')) {
      // Comments sort by (createdAt, id): two made in the same millisecond
      // fall back to their random ids, on either server.
      const sidecar = parseMaybeJson(text) as { comments?: { body: string }[] }
      sidecar.comments?.sort((a, b) => a.body.localeCompare(b.body))
      files[norm.string(rel)] = norm.value(sidecar)
    } else if (rel.endsWith('.json')) files[norm.string(rel)] = norm.value(parseMaybeJson(text))
    else if (Buffer.from(text, 'utf8').equals(bytes)) files[norm.string(rel)] = norm.string(text)
    else files[norm.string(rel)] = { base64: bytes.toString('base64'), size: statSync(abs).size }
  }
  return files
}

interface Run {
  instructions: string
  tools: unknown[]
  steps: StepRecord[]
  files: Record<string, unknown>
  exit?: { code: number | null; stderr: string[] }
}

async function runOnce(command: string[], cwd: string | undefined, steps: Step[], session?: Session): Promise<Run> {
  const { tmp, vault, env } = prepare(session)
  const stderr: string[] = []
  const proc = Bun.spawn([...command, ...(session?.args ?? [])], {
    cwd,
    env,
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe'
  })
  const drained = (async () => {
    for await (const chunk of proc.stderr as ReadableStream<Uint8Array>) stderr.push(new TextDecoder().decode(chunk))
  })()
  const norm = new Normalizer(vault)
  if (session?.expectExit) {
    try {
      const timer = setTimeout(() => proc.kill(), 15000)
      const code = await proc.exited
      clearTimeout(timer)
      await drained
      const lines = stderr
        .join('')
        .split('\n')
        .filter((l) => l.startsWith('[zennotes-mcp]'))
        .map((l) => norm.string(l))
      return { instructions: '', tools: [], steps: [], files: {}, exit: { code, stderr: lines } }
    } finally {
      rmSync(tmp, { recursive: true, force: true })
    }
  }
  const client = new StdioClient(proc)
  try {
    const init = await client.request('initialize', {
      protocolVersion: '2025-06-18',
      capabilities: {},
      clientInfo: { name: 'claude-code', version: '1.0.0' }
    })
    if (!init.result) throw new Error(`initialize failed: ${JSON.stringify(init)}\n${stderr.join('')}`)
    client.notify('notifications/initialized')
    const listed = await client.request('tools/list', {})
    const tools = ((listed.result as { tools?: unknown[] })?.tools ?? []) as unknown[]

    const labelled = new Map<string, unknown>()
    const records: StepRecord[] = []
    for (const step of steps) {
      const args = resolveRefs(step.args, labelled)
      const reply = await client.request('tools/call', { name: step.tool, arguments: args })
      const result = reply.result as { content?: Record<string, unknown>[]; isError?: boolean } | undefined
      if (!result) throw new Error(`${step.tool}: protocol error ${JSON.stringify(reply.error)}\n${stderr.join('')}`)
      let parsed = recordContent(result.content ?? [])
      const field = (x: unknown, key: string): unknown => (x as Record<string, unknown>)[key]
      const target = step.sortAt ? field(parsed, step.sortAt) : parsed
      if (step.sortBy && Array.isArray(target)) {
        const keys = step.sortBy.split(',')
        const sorted = [...target].sort((a, b) => {
          for (const key of keys) {
            const [x, y] = [field(a, key), field(b, key)]
            const order = typeof x === 'number' && typeof y === 'number' ? x - y : String(x).localeCompare(String(y))
            if (order !== 0) return order
          }
          return 0
        })
        parsed = step.sortAt ? { ...(parsed as object), [step.sortAt]: sorted } : sorted
      }
      // After sorting, so a `$ref` to `0.id` means the same item on both servers.
      if (step.label) labelled.set(step.label, parsed)
      records.push({
        tool: step.tool,
        args: norm.value(args) as Record<string, unknown>,
        isError: result.isError === true,
        result: norm.value(parsed)
      })
    }

    const instructions = (init.result as { instructions?: string }).instructions ?? ''
    return { instructions, tools, steps: records, files: snapshot(vault, norm) }
  } finally {
    await client.close()
    rmSync(tmp, { recursive: true, force: true })
  }
}

export async function drive(command: string[], cwd?: string): Promise<Transcript> {
  const scenario = JSON.parse(readFileSync(path.join(HERE, 'scenario.json'), 'utf8')) as Scenario
  const main = await runOnce(command, cwd, scenario.steps)
  const sessions: SessionRecord[] = []
  for (const session of scenario.sessions ?? []) {
    const run = await runOnce(command, cwd, session.steps, session)
    const suffix = run.instructions.startsWith(main.instructions)
      ? run.instructions.slice(main.instructions.length)
      : run.instructions
    sessions.push({
      label: session.label,
      args: session.args ?? [],
      instructionsSuffix: run.exit ? '' : suffix,
      tools: run.tools.map((t) => (t as { name: string }).name),
      steps: run.steps,
      files: run.files,
      ...(run.exit ? { exit: run.exit } : {})
    })
  }
  return { instructions: main.instructions, tools: main.tools, steps: main.steps, files: main.files, sessions }
}
