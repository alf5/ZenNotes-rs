import { convertFileSrc } from '@tauri-apps/api/core'

/**
 * Port of the Electron preload's `resolveLocalAssetUrl` /
 * `resolveVaultAssetUrl` (apps/desktop/src/preload/index.ts). Resolves a
 * markdown image/link href to a URL the webview can load through the
 * custom `syn-asset` URI scheme registered by the Rust backend (M7).
 *
 * The path math is pure POSIX string logic (no Node `path` in the
 * webview). Everything is validated to stay inside the vault root before
 * a URL is produced; out-of-vault references return null.
 */

const ASSET_SCHEME = 'syn-asset'

function stripQueryAndHash(value: string): string {
  const hashIdx = value.indexOf('#')
  const queryIdx = value.indexOf('?')
  const cutIdx =
    hashIdx === -1
      ? queryIdx
      : queryIdx === -1
        ? hashIdx
        : Math.min(hashIdx, queryIdx)
  return cutIdx === -1 ? value : value.slice(0, cutIdx)
}

function decodeHrefPath(value: string): string {
  const cleaned = stripQueryAndHash(value)
  try {
    return decodeURIComponent(cleaned)
  } catch {
    return cleaned
  }
}

/** POSIX `path.normalize` for the limited shapes we encounter here. */
function posixNormalize(input: string): string {
  const isAbsolute = input.startsWith('/')
  const segments = input.split('/')
  const out: string[] = []
  for (const seg of segments) {
    if (seg === '' || seg === '.') continue
    if (seg === '..') {
      if (out.length > 0 && out[out.length - 1] !== '..') out.pop()
      else if (!isAbsolute) out.push('..')
      continue
    }
    out.push(seg)
  }
  const joined = out.join('/')
  return isAbsolute ? '/' + joined : joined || '.'
}

function posixDirname(p: string): string {
  const idx = p.lastIndexOf('/')
  if (idx === -1) return '.'
  if (idx === 0) return '/'
  return p.slice(0, idx)
}

function resolveVaultRelativeAssetPath(notePath: string, href: string): string | null {
  const trimmed = href.trim()
  if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('//')) return null
  if (/^[a-zA-Z][a-zA-Z\d+.-]*:/.test(trimmed)) return null

  const normalizedNotePath = notePath.split('\\').join('/')
  const noteDir = posixDirname(normalizedNotePath)
  const decodedHref = decodeHrefPath(trimmed)
  const relativeTarget = decodedHref.startsWith('/')
    ? decodedHref.replace(/^\/+/, '')
    : posixNormalize(`${noteDir === '.' ? '' : noteDir}/${decodedHref}`)
  if (relativeTarget === '..' || relativeTarget.startsWith('../')) return null
  return relativeTarget
}

/** Join a vault root (absolute, native separators) with a POSIX relative path. */
function joinVaultPath(vaultRoot: string, relativePosix: string): string {
  const sep = vaultRoot.includes('\\') && !vaultRoot.includes('/') ? '\\' : '/'
  const root = vaultRoot.replace(/[\\/]+$/, '')
  const rel = relativePosix.split('/').join(sep)
  return `${root}${sep}${rel}`
}

function assetUrlForAbsolutePath(absPath: string): string {
  // convertFileSrc yields the platform-correct origin for the custom
  // scheme: `syn-asset://<path>` on macOS, `http://syn-asset.localhost/<path>`
  // on Windows/Linux. The Rust scheme handler decodes it back to a file.
  return convertFileSrc(absPath, ASSET_SCHEME)
}

export function resolveLocalAssetUrl(
  vaultRoot: string,
  notePath: string,
  href: string
): string | null {
  const trimmed = href.trim()
  if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('//')) return null
  if (/^[a-zA-Z][a-zA-Z\d+.-]*:/.test(trimmed)) return null
  const relativeTarget = resolveVaultRelativeAssetPath(notePath, href)
  if (!relativeTarget) return null
  return assetUrlForAbsolutePath(joinVaultPath(vaultRoot, relativeTarget))
}

export function resolveVaultAssetUrl(vaultRoot: string, assetPath: string): string | null {
  const trimmed = assetPath.trim()
  if (!trimmed) return null
  return assetUrlForAbsolutePath(joinVaultPath(vaultRoot, trimmed.split('\\').join('/')))
}
