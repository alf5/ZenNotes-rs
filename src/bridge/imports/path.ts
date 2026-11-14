/**
 * POSIX path helpers, as Node's `path.posix`, for the import run code
 * (the port's copies of upstream's `*-import-run.ts`). Every path that code
 * handles is a virtual one (`/<kind>/<place>/…`, see fs.ts), always absolute
 * and always with `/`, so this is all of `node:path` it needs.
 */

export const sep = '/'

export function isAbsolute(p: string): boolean {
  return p.startsWith('/')
}

function normalizeParts(parts: string[], absolute: boolean): string[] {
  const out: string[] = []
  for (const part of parts) {
    if (!part || part === '.') continue
    if (part === '..') {
      if (out.length > 0 && out[out.length - 1] !== '..') out.pop()
      else if (!absolute) out.push('..')
    } else {
      out.push(part)
    }
  }
  return out
}

export function normalize(p: string): string {
  if (!p) return '.'
  const absolute = isAbsolute(p)
  const body = normalizeParts(p.split('/'), absolute).join('/')
  if (absolute) return `/${body}`
  return body || '.'
}

export function join(...segments: string[]): string {
  const joined = segments.filter((segment) => segment.length > 0).join('/')
  return joined ? normalize(joined) : '.'
}

/** Absolute, from the right: there is no working folder to start from. */
export function resolve(...segments: string[]): string {
  let resolved = ''
  for (let i = segments.length - 1; i >= 0 && !isAbsolute(resolved); i--) {
    const segment = segments[i]
    if (!segment) continue
    resolved = resolved ? `${segment}/${resolved}` : segment
  }
  return normalize(isAbsolute(resolved) ? resolved : `/${resolved}`)
}

export function dirname(p: string): string {
  if (!p) return '.'
  const trimmed = p.replace(/\/+$/, '')
  if (!trimmed) return '/'
  const slash = trimmed.lastIndexOf('/')
  if (slash === -1) return '.'
  if (slash === 0) return '/'
  return trimmed.slice(0, slash).replace(/\/+$/, '') || '/'
}

export function basename(p: string, ext?: string): string {
  const trimmed = p.replace(/\/+$/, '')
  const base = trimmed.slice(trimmed.lastIndexOf('/') + 1)
  if (ext && base !== ext && base.endsWith(ext)) return base.slice(0, -ext.length)
  return base
}

export function relative(from: string, to: string): string {
  const a = resolve(from).split('/').filter(Boolean)
  const b = resolve(to).split('/').filter(Boolean)
  let same = 0
  while (same < a.length && same < b.length && a[same] === b[same]) same++
  return [...Array<string>(a.length - same).fill('..'), ...b.slice(same)].join('/')
}

const path = { sep, isAbsolute, normalize, join, resolve, dirname, basename, relative }
export default path
