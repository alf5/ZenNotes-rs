/**
 * Custom themes + CSS overrides — the webview half over the Rust
 * `custom_themes_*` / `overrides_*` commands (src-tauri/src/custom_css.rs).
 *
 * Mirrors upstream `apps/desktop/src/main/{custom-themes,overrides}.ts`:
 * Rust scans/watches the dirs; manifest parsing (`parseThemeManifest`) and
 * new-theme scaffolding (`scaffoldThemeCss`) run here through the vendored
 * shared-domain functions, so validation and generated CSS stay identical to
 * upstream. Change events arrive as content-free pings; we re-scan and fan
 * the fresh lists out to subscribers (the store expects full lists).
 */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import {
  parseThemeManifest,
  scaffoldThemeCss,
  type CustomTheme,
  type CustomThemePalette
} from '@zennotes/shared-domain/custom-themes'
import type { Override } from '@zennotes/shared-domain/overrides'

interface RawThemeEntry {
  slug: string
  css: string | null
  manifest: string | null
}

// Starter palette for "New theme" (upstream main/custom-themes.ts:97).
const NEW_THEME_LIGHT: CustomThemePalette = { bg: '#ffffff', text: '#1d1d1f', accent: '#007aff' }
const NEW_THEME_DARK: CustomThemePalette = { bg: '#1c1c1e', text: '#ffffff', accent: '#0a84ff' }

function slugify(name: string): string {
  return (
    name
      .toLowerCase()
      .trim()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 64) || 'my-theme'
  )
}

export async function listCustomThemes(): Promise<CustomTheme[]> {
  const entries = await invoke<RawThemeEntry[]>('custom_themes_scan')
  const themes: CustomTheme[] = []
  for (const entry of entries) {
    if (typeof entry.css === 'string') {
      let manifestRaw: unknown = null
      if (entry.manifest !== null) {
        try {
          manifestRaw = JSON.parse(entry.manifest)
        } catch {
          /* manifest optional — fall back to slug-named defaults */
        }
      }
      const m = parseThemeManifest(manifestRaw, entry.slug)
      themes.push({
        slug: entry.slug,
        name: m.name,
        author: m.author,
        version: m.version,
        description: m.description,
        modes: m.modes,
        css: entry.css,
        preview: m.preview
      })
    } else {
      themes.push({
        slug: entry.slug,
        name: entry.slug,
        modes: 'both',
        css: '',
        error: 'No readable theme.css here. Add one (see README.md) or use New theme.'
      })
    }
  }
  themes.sort((a, b) => a.name.localeCompare(b.name))
  return themes
}

export function getCustomThemesDir(): Promise<string> {
  return invoke('custom_themes_dir_path')
}

export function revealCustomThemesDir(slug?: string): Promise<void> {
  return invoke('custom_themes_reveal', { slug: slug ?? null })
}

export function deleteCustomTheme(slug: string): Promise<void> {
  return invoke('custom_themes_delete', { slug })
}

/** Scaffold a new theme folder from the neutral starter palette. Returns the
 *  slug (unique within the themes dir) or null on failure. */
export async function createCustomTheme(input: { name?: string }): Promise<string | null> {
  const name = input?.name?.trim() || 'My Theme'
  try {
    // Rust reserves the unique folder first so the scaffolded CSS (whose
    // header embeds the slug) is generated with the final, deduped slug.
    const slug = await invoke<string>('custom_themes_reserve', { slugBase: slugify(name) })
    const manifest =
      JSON.stringify({ name, author: '', version: '1.0.0', description: '', modes: 'both' }, null, 2) +
      '\n'
    const css = scaffoldThemeCss({ name, slug, light: NEW_THEME_LIGHT, dark: NEW_THEME_DARK })
    await invoke('custom_themes_write_files', { slug, manifest, css })
    return slug
  } catch {
    return null
  }
}

export function listOverrides(): Promise<Override[]> {
  return invoke('overrides_list')
}

export function revealOverridesDir(name?: string): Promise<void> {
  return invoke('overrides_reveal', { name: name ?? null })
}

export function deleteOverride(name: string): Promise<void> {
  return invoke('overrides_delete', { name })
}

// ---------------------------------------------------------------------------
// Change subscriptions — one Tauri listener each, fanned out to subscribers.
// ---------------------------------------------------------------------------

const themeSubscribers = new Set<(themes: CustomTheme[]) => void>()
let themesListenerStarted = false

export function subscribeCustomThemesChange(cb: (themes: CustomTheme[]) => void): () => void {
  themeSubscribers.add(cb)
  if (!themesListenerStarted) {
    themesListenerStarted = true
    void listen('custom-themes://changed', () => {
      void listCustomThemes()
        .then((themes) => {
          for (const sub of themeSubscribers) sub(themes)
        })
        .catch(() => {})
    })
  }
  return () => {
    themeSubscribers.delete(cb)
  }
}

const overrideSubscribers = new Set<(overrides: Override[]) => void>()
let overridesListenerStarted = false

export function subscribeOverridesChange(cb: (overrides: Override[]) => void): () => void {
  overrideSubscribers.add(cb)
  if (!overridesListenerStarted) {
    overridesListenerStarted = true
    void listen('overrides://changed', () => {
      void listOverrides()
        .then((overrides) => {
          for (const sub of overrideSubscribers) sub(overrides)
        })
        .catch(() => {})
    })
  }
  return () => {
    overrideSubscribers.delete(cb)
  }
}
