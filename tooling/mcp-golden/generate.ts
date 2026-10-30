/**
 * Record upstream's MCP server as the reference `zennotes-rs mcp` is held to.
 *
 *   bun tooling/mcp-golden/generate.ts <path to an upstream ZenNotes clone>
 *
 * Run it on every re-vendor, then `bun run test:mcp`. It writes:
 *  - tooling/mcp-golden/golden.json: upstream's replies to scenario.json and
 *    the vault it leaves behind;
 *  - src-tauri/src/mcp/tools.json: the tool list, served verbatim by the Rust
 *    server, so names, descriptions and schemas cannot drift;
 *  - src-tauri/src/mcp/instructions.md: the default server instructions.
 *
 * Upstream's server runs straight from its TypeScript sources under bun: they
 * are copied to a temp dir (the clone is never touched) and given only the npm
 * packages the MCP entry imports.
 */

import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { drive, GOLDEN_FILE, HERE } from './drive'

const RUST_MCP_DIR = path.join(HERE, '..', '..', 'src-tauri', 'src', 'mcp')
/** npm packages the MCP entry's import graph reaches. A missing one fails the
 *  run with "Cannot find package"; add it here. */
const PACKAGES = ['@modelcontextprotocol/sdk', 'smol-toml', 'lz-string']

const upstream = process.argv[2]
if (!upstream || !existsSync(path.join(upstream, 'apps/desktop/src/mcp/index.ts'))) {
  console.error('usage: bun tooling/mcp-golden/generate.ts <upstream ZenNotes clone>')
  process.exit(2)
}

function versionOf(pkg: string): string {
  for (const manifest of ['apps/desktop/package.json', 'packages/shared-domain/package.json', 'package.json']) {
    const file = path.join(upstream, manifest)
    if (!existsSync(file)) continue
    const json = JSON.parse(readFileSync(file, 'utf8')) as Record<string, Record<string, string> | undefined>
    const range = json.dependencies?.[pkg] ?? json.devDependencies?.[pkg]
    if (range) return range
  }
  return 'latest'
}

const snapshot = mkdtempSync(path.join(os.tmpdir(), 'zn-upstream-'))
try {
  mkdirSync(path.join(snapshot, 'apps/desktop'), { recursive: true })
  for (const rel of ['apps/desktop/src', 'apps/desktop/tsconfig.node.json', 'tsconfig.base.json']) {
    cpSync(path.join(upstream, rel), path.join(snapshot, rel), { recursive: true })
  }
  for (const pkg of ['shared-domain', 'bridge-contract']) {
    for (const rel of ['src', 'package.json']) {
      cpSync(path.join(upstream, 'packages', pkg, rel), path.join(snapshot, 'packages', pkg, rel), { recursive: true })
    }
  }

  const deps = path.join(snapshot, '.deps')
  mkdirSync(deps)
  writeFileSync(path.join(deps, 'package.json'), JSON.stringify({ name: 'zn-mcp-golden-deps', private: true }))
  const install = Bun.spawnSync(['bun', 'add', ...PACKAGES.map((p) => `${p}@${versionOf(p)}`)], {
    cwd: deps,
    stdout: 'inherit',
    stderr: 'inherit'
  })
  if (install.exitCode !== 0) throw new Error('bun add failed')
  symlinkSync(path.join(deps, 'node_modules'), path.join(snapshot, 'node_modules'))

  const desktop = path.join(snapshot, 'apps/desktop')
  const transcript = await drive(['bun', '--tsconfig-override', 'tsconfig.node.json', 'src/mcp/index.ts'], desktop)

  writeFileSync(
    GOLDEN_FILE,
    JSON.stringify({ steps: transcript.steps, files: transcript.files }, null, 2) + '\n'
  )
  writeFileSync(path.join(RUST_MCP_DIR, 'tools.json'), JSON.stringify(transcript.tools, null, 2) + '\n')
  writeFileSync(path.join(RUST_MCP_DIR, 'instructions.md'), transcript.instructions)
  const errors = transcript.steps.filter((s) => s.isError).length
  console.log(
    `golden.json: ${transcript.steps.length} steps (${errors} errors), ${Object.keys(transcript.files).length} files; ` +
      `tools.json: ${transcript.tools.length} tools`
  )
} finally {
  rmSync(snapshot, { recursive: true, force: true })
}
