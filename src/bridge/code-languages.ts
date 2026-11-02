/**
 * User-installed TextMate code languages — the webview half over the Rust
 * `code_languages_*` commands (src-tauri/src/code_languages.rs).
 *
 * Same split as custom-css.ts: Rust owns the directory
 * (`~/.config/zennotes/languages/<id>/{manifest.json,grammar.tmLanguage.json}`)
 * and watches it; parsing and validation run here through the vendored
 * shared-domain functions, so what counts as a valid manifest, a legal fence
 * tag or a reserved alias stays identical to upstream. Change events are
 * content-free pings; we re-scan and fan a fresh list out to subscribers.
 *
 * Mirrors upstream `apps/desktop/src/main/custom-code-languages.ts` with one
 * documented difference: upstream also VETS each grammar by tokenizing a
 * corpus in a worker with a 2 s ceiling, so a catastrophically backtracking
 * regex cannot hang the editor. That runs on node worker_threads there and has
 * no equivalent here yet — see the "Grammar vetting" row in GAP-ANALYSIS.md.
 */

import { invoke } from './ipc'
import { listen } from '@tauri-apps/api/event'
import {
  RESERVED_CODE_FENCE_TAGS,
  normalizeCodeFenceTag,
  parseTextMateGrammar,
  validateCustomCodeLanguageManifest,
  type CustomCodeLanguage,
  type CustomCodeLanguageInstallInput,
  type CustomCodeLanguageManifest,
  type CustomCodeLanguageUpdateInput
} from '@zennotes/shared-domain/custom-code-languages'

/** One language folder, unparsed. `null` means the file was missing. */
interface RawLanguageEntry {
  id: string
  manifest: string | null
  grammar: string | null
}

const CHANGED_EVENT = 'code-languages://changed'

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : 'This language could not be loaded.'
}

/**
 * The fallback record for a folder we could not read. Upstream shows the
 * language in Settings with its error rather than hiding it, so a broken
 * install is something the user can see and remove.
 */
function fallbackFor(id: string, error: unknown): CustomCodeLanguage {
  return {
    schemaVersion: 1,
    id,
    name: id,
    aliases: [id],
    scopeName: `source.${id}`,
    enabled: false,
    grammar: '',
    error: messageOf(error)
  }
}

export async function list(): Promise<CustomCodeLanguage[]> {
  const entries = await invoke<RawLanguageEntry[]>('code_languages_scan')
  const out: CustomCodeLanguage[] = []
  for (const entry of entries) {
    try {
      if (entry.grammar === null) throw new Error('This language has no grammar file.')
      if (entry.manifest === null) throw new Error('This language has no manifest.json.')
      const parsed = parseTextMateGrammar(entry.grammar)
      const raw = JSON.parse(entry.manifest) as Partial<CustomCodeLanguageManifest>
      const manifest = validateCustomCodeLanguageManifest(
        {
          id: String(raw.id ?? entry.id),
          name: String(raw.name ?? entry.id),
          aliases: Array.isArray(raw.aliases)
            ? raw.aliases.filter((v): v is string => typeof v === 'string')
            : [entry.id],
          scopeName: parsed.scopeName,
          enabled: raw.enabled !== false
        },
        { reservedAliases: RESERVED_CODE_FENCE_TAGS }
      )
      // The folder name is the id everywhere else (fence tags resolve to it),
      // so a manifest that disagrees is broken rather than merely stale.
      if (manifest.id !== entry.id) throw new Error('The manifest id must match its folder name.')
      out.push({ ...manifest, grammar: entry.grammar })
    } catch (error) {
      out.push(fallbackFor(entry.id, error))
    }
  }
  out.sort((a, b) => a.name.localeCompare(b.name))
  return out
}

export async function install(
  input: CustomCodeLanguageInstallInput
): Promise<CustomCodeLanguage> {
  const parsed = parseTextMateGrammar(input.grammar)
  const existing = await list()
  // Normalize before the replace guard: an unnormalized id over IPC ("Gleam")
  // resolves to the same folder but would miss the guard (upstream's fix).
  const id = normalizeCodeFenceTag(input.id)
  const manifest = validateCustomCodeLanguageManifest(
    {
      id,
      name: input.name,
      aliases: input.aliases,
      scopeName: parsed.scopeName,
      enabled: input.enabled !== false
    },
    {
      reservedAliases: RESERVED_CODE_FENCE_TAGS,
      existing,
      replacingId: input.replace ? id : undefined
    }
  )
  if (!input.replace && existing.some((lang) => lang.id === manifest.id)) {
    throw new Error(`A language with the fence tag "${manifest.id}" is already installed.`)
  }
  await invoke('code_languages_write', {
    id: manifest.id,
    manifest: JSON.stringify(manifest, null, 2),
    grammar: input.grammar
  })
  return { ...manifest, grammar: input.grammar }
}

export async function update(input: CustomCodeLanguageUpdateInput): Promise<CustomCodeLanguage> {
  const existing = await list()
  const current = existing.find((lang) => lang.id === normalizeCodeFenceTag(input.id))
  if (!current) throw new Error(`No installed language with the fence tag "${input.id}".`)
  const manifest = validateCustomCodeLanguageManifest(
    {
      id: current.id,
      name: input.name ?? current.name,
      aliases: input.aliases ?? current.aliases,
      scopeName: current.scopeName,
      enabled: input.enabled ?? current.enabled
    },
    {
      reservedAliases: RESERVED_CODE_FENCE_TAGS,
      existing,
      replacingId: current.id
    }
  )
  await invoke('code_languages_write_manifest', {
    id: manifest.id,
    manifest: JSON.stringify(manifest, null, 2)
  })
  return { ...manifest, grammar: current.grammar }
}

export function remove(id: string): Promise<void> {
  return invoke('code_languages_delete', { id: normalizeCodeFenceTag(id) })
}

export function revealDir(id?: string): Promise<void> {
  return invoke('code_languages_reveal', { id: id ? normalizeCodeFenceTag(id) : null })
}

export function subscribe(cb: (next: CustomCodeLanguage[]) => void): () => void {
  let unlisten = (): void => {}
  let cancelled = false
  void listen(CHANGED_EVENT, () => {
    void list().then(cb).catch(() => {})
  }).then((fn) => {
    if (cancelled) fn()
    else unlisten = fn
  })
  return () => {
    cancelled = true
    unlisten()
  }
}
