/**
 * Hold `zennotes-rs mcp` to upstream's MCP server: replay scenario.json
 * against the Rust binary and compare every reply, and the vault it leaves
 * behind, with golden.json (recorded from upstream by generate.ts).
 *
 *   bun run test:mcp                         # target/debug/zennotes-rs
 *   ZENNOTES_RS_BIN=/path/to/zennotes-rs bun tooling/mcp-golden/check.ts
 *
 * Build first (`cargo build` in src-tauri). Exits 1 on any difference.
 */

import { existsSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { drive, GOLDEN_FILE, HERE } from './drive'

/** Where upstream and this port knowingly part ways, by step index: what the
 *  port answers instead and why. Keep this list short and explained. */
const KNOWN_DIVERGENCES: Record<number, string> = {}

function binary(): string {
  if (process.env.ZENNOTES_RS_BIN) return process.env.ZENNOTES_RS_BIN
  const target = process.env.CARGO_TARGET_DIR ?? path.join(HERE, '..', '..', 'src-tauri', 'target')
  return path.join(target, 'debug', process.platform === 'win32' ? 'zennotes-rs.exe' : 'zennotes-rs')
}

function diff(expected: unknown, actual: unknown, at: string, out: string[]): void {
  if (out.length >= 12) return
  if (Array.isArray(expected) && Array.isArray(actual)) {
    if (expected.length !== actual.length) out.push(`${at}: length ${actual.length}, expected ${expected.length}`)
    for (let i = 0; i < Math.min(expected.length, actual.length); i++) diff(expected[i], actual[i], `${at}[${i}]`, out)
    return
  }
  if (expected && actual && typeof expected === 'object' && typeof actual === 'object') {
    const e = expected as Record<string, unknown>
    const a = actual as Record<string, unknown>
    for (const key of new Set([...Object.keys(e), ...Object.keys(a)])) {
      if (!(key in a)) out.push(`${at}.${key}: missing, expected ${JSON.stringify(e[key])?.slice(0, 160)}`)
      else if (!(key in e)) out.push(`${at}.${key}: unexpected ${JSON.stringify(a[key])?.slice(0, 160)}`)
      else diff(e[key], a[key], `${at}.${key}`, out)
    }
    return
  }
  if (JSON.stringify(expected) !== JSON.stringify(actual)) {
    out.push(`${at}: ${JSON.stringify(actual)?.slice(0, 200)}\n      expected ${JSON.stringify(expected)?.slice(0, 200)}`)
  }
}

const bin = binary()
if (!existsSync(bin)) {
  console.error(`No binary at ${bin}. Run \`cargo build\` in src-tauri, or set ZENNOTES_RS_BIN.`)
  process.exit(2)
}
const golden = JSON.parse(readFileSync(GOLDEN_FILE, 'utf8')) as Awaited<ReturnType<typeof drive>>
const actual = await drive([bin, 'mcp'])

let failures = 0
golden.steps.forEach((expected, i) => {
  const got = actual.steps[i]
  const problems: string[] = []
  if (got.isError !== expected.isError) problems.push(`isError ${got.isError}, expected ${expected.isError}`)
  diff(expected.result, got.result, 'result', problems)
  if (problems.length === 0) return
  if (KNOWN_DIVERGENCES[i]) {
    console.log(`~ step ${i} ${expected.tool}: known divergence (${KNOWN_DIVERGENCES[i]})`)
    return
  }
  failures++
  console.log(`✗ step ${i} ${expected.tool} ${JSON.stringify(expected.args)}`)
  for (const p of problems) console.log(`    ${p}`)
})

const fileProblems: string[] = []
diff(golden.files, actual.files, 'files', fileProblems)
if (fileProblems.length > 0) {
  failures++
  console.log('✗ vault contents after the scenario')
  for (const p of fileProblems) console.log(`    ${p}`)
}

const tools = JSON.parse(readFileSync(path.join(HERE, '..', '..', 'src-tauri', 'src', 'mcp', 'tools.json'), 'utf8'))
const toolProblems: string[] = []
diff(tools, actual.tools, 'tools', toolProblems)
if (toolProblems.length > 0) {
  failures++
  console.log('✗ tools/list differs from src-tauri/src/mcp/tools.json')
  for (const p of toolProblems) console.log(`    ${p}`)
}

console.log(failures === 0 ? `✓ ${golden.steps.length} steps match upstream` : `${failures} mismatches`)
process.exit(failures === 0 ? 0 : 1)
