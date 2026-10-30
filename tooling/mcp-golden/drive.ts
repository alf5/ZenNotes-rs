/**
 * Drives one MCP server over stdio through scenario.json and returns what it
 * said, normalized so two servers can be compared: upstream's TypeScript
 * server (generate.ts records it as golden.json) and `zennotes-rs mcp`
 * (check.ts compares against it).
 *
 * Both servers run on a fresh copy of ./vault with every file's mtime pinned,
 * under a throwaway HOME / config dir, with ZENNOTES_VAULT naming the vault, so
 * nothing on this machine leaks in and nothing outside the copy is written.
 */

import { cpSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, utimesSync } from 'node:fs'
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

function prepare(): { tmp: string; vault: string; env: Record<string, string> } {
  const tmp = mkdtempSync(path.join(os.tmpdir(), 'zn-mcp-golden-'))
  const vault = path.join(tmp, 'vault')
  cpSync(FIXTURE_VAULT, vault, { recursive: true })
  listFiles(vault).forEach((rel, i) => {
    const at = new Date(FIXTURE_EPOCH_MS + i * 60_000)
    utimesSync(path.join(vault, rel), at, at)
  })
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
      TZ: 'UTC'
    }
  }
}

/* ---------- Normalization ------------------------------------------------ */

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

class Normalizer {
  private ids = new Map<string, string>()
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

  string(s: string): string {
    if (UUID_RE.test(s)) {
      if (!this.ids.has(s)) this.ids.set(s, `<id${this.ids.size + 1}>`)
      return this.ids.get(s)!
    }
    return s.split(this.root).join('<ROOT>')
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

export async function drive(command: string[], cwd?: string): Promise<Transcript> {
  const scenario = JSON.parse(readFileSync(path.join(HERE, 'scenario.json'), 'utf8')) as { steps: Step[] }
  const { tmp, vault, env } = prepare()
  const stderr: string[] = []
  const proc = Bun.spawn(command, { cwd, env, stdin: 'pipe', stdout: 'pipe', stderr: 'pipe' })
  void (async () => {
    for await (const chunk of proc.stderr as ReadableStream<Uint8Array>) stderr.push(new TextDecoder().decode(chunk))
  })()
  const client = new StdioClient(proc)
  const norm = new Normalizer(vault)
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
    const steps: StepRecord[] = []
    for (const step of scenario.steps) {
      const args = resolveRefs(step.args, labelled)
      const reply = await client.request('tools/call', { name: step.tool, arguments: args })
      const result = reply.result as { content?: { type: string; text: string }[]; isError?: boolean } | undefined
      if (!result) throw new Error(`${step.tool}: protocol error ${JSON.stringify(reply.error)}\n${stderr.join('')}`)
      const text = (result.content ?? []).map((c) => c.text).join('')
      let parsed = parseMaybeJson(text)
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
      steps.push({
        tool: step.tool,
        args: norm.value(args) as Record<string, unknown>,
        isError: result.isError === true,
        result: norm.value(parsed)
      })
    }

    const files: Record<string, unknown> = {}
    for (const rel of listFiles(vault)) {
      const abs = path.join(vault, rel)
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

    const instructions = (init.result as { instructions?: string }).instructions ?? ''
    return { instructions, tools, steps, files }
  } finally {
    await client.close()
    rmSync(tmp, { recursive: true, force: true })
  }
}
