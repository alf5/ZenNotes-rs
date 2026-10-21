/**
 * Naming for pasted images, shared by the desktop main process and the web
 * client so a paste produces the same filename against a local vault, a
 * remote workspace, and the self-hosted web app.
 *
 * The stem is scrubbed of path separators, cross-platform-reserved
 * characters, and the characters that break the `![[...]]` wikilink embed
 * the paste inserts ([ ] # ^). The server's own upload cleaning leaves the
 * wikilink-breaking ones in, so the scrub must happen on the client that
 * writes the link.
 */

export interface PastedImageNameInput {
  mimeType: string
  suggestedName?: string | null
}

export const IMAGE_FILE_EXTENSIONS = new Set([
  '.apng',
  '.avif',
  '.gif',
  '.jpeg',
  '.jpg',
  '.png',
  '.svg',
  '.webp'
])

export const PASTED_IMAGE_MIME_EXTENSIONS: Record<string, string> = {
  'image/apng': '.apng',
  'image/avif': '.avif',
  'image/gif': '.gif',
  'image/jpeg': '.jpg',
  'image/jpg': '.jpg',
  'image/png': '.png',
  'image/svg+xml': '.svg',
  'image/webp': '.webp'
}

function pad(value: number): string {
  return String(value).padStart(2, '0')
}

function pastedImageTimestamp(now: Date): string {
  const date = [now.getFullYear(), pad(now.getMonth() + 1), pad(now.getDate())].join('-')
  const time = [pad(now.getHours()), pad(now.getMinutes()), pad(now.getSeconds())].join('')
  return `${date} ${time}`
}

/** Last path segment, treating both separators as such (a Windows client
 *  can suggest a backslashed name to a posix host). */
function baseName(name: string): string {
  const segments = name.split(/[\\/]/)
  return segments[segments.length - 1] ?? ''
}

/** Extension with the dot, '' when there is none. A leading dot alone
 *  (dotfiles) is a name, not an extension, matching node's path.extname. */
function extName(name: string): string {
  const base = baseName(name)
  const dot = base.lastIndexOf('.')
  return dot > 0 ? base.slice(dot) : ''
}

export function pastedImageExtension(input: PastedImageNameInput): string {
  const suggestedExt = extName(input.suggestedName ?? '').toLowerCase()
  if (IMAGE_FILE_EXTENSIONS.has(suggestedExt)) return suggestedExt

  const mimeExt = PASTED_IMAGE_MIME_EXTENSIONS[input.mimeType.toLowerCase()]
  if (mimeExt) return mimeExt
  if (input.mimeType.toLowerCase().startsWith('image/')) return '.png'
  throw new Error('Clipboard item is not an image.')
}

/** Chromium, which the desktop app and Android's WebView run on, names every
 *  bitmap pasted from the clipboard `image.png`. Kept as the name, it made
 *  every paste `image.png`, then `image (2).png` and on (#775), so that stem
 *  counts as no name and a paste gets the timestamped one. A file copied from
 *  the Finder or Explorer arrives under its own name and keeps it. */
function isClipboardPlaceholderName(base: string): boolean {
  return base.toLowerCase() === 'image'
}

export function pastedImageFilename(input: PastedImageNameInput, now: Date): string {
  const ext = pastedImageExtension(input)
  const rawName = baseName(input.suggestedName ?? '')
  const nameExt = extName(rawName)
  const rawBase = nameExt ? rawName.slice(0, rawName.length - nameExt.length) : rawName
  const base = rawBase
    .replace(/[\\/:%*?"<>|[\]#^]/g, '-')
    .replace(/\s+/g, ' ')
    .trim()
  const fallbackBase = `Pasted Image ${pastedImageTimestamp(now)}`
  const finalBase =
    base && base !== '.' && base !== '..' && !isClipboardPlaceholderName(base) ? base : fallbackBase
  return `${finalBase}${ext}`
}
