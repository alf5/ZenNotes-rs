/**
 * A heading anchor the way people write it, matched against a note's headings.
 *
 * `[[#Section Three]]` names the heading by its text, the Obsidian way. A
 * Markdown link has to escape the space, `[jump](#Section%20Three)`, or uses
 * the slug GitHub and most Markdown tools generate, `[jump](#section-three)`.
 * Matching only the exact text left both Markdown forms going nowhere: the
 * reading view had no element with that id to scroll to, and the editor looked
 * for a heading literally named "section-three".
 *
 * Tried in order, first hit wins, so an exact name always beats a slug that
 * happens to collide with it: the text itself (case-insensitive, as before),
 * the URL-decoded text, then the GitHub-style slug of both sides.
 */
export function findHeadingForAnchor<T extends { text: string }>(
  headings: readonly T[],
  anchor: string
): T | undefined {
  const raw = anchor.trim().toLowerCase()
  if (!raw) return undefined
  const exact = headings.find((h) => h.text.trim().toLowerCase() === raw)
  if (exact) return exact
  const decoded = safeDecode(anchor).trim().toLowerCase()
  if (decoded !== raw) {
    const byDecoded = headings.find((h) => h.text.trim().toLowerCase() === decoded)
    if (byDecoded) return byDecoded
  }
  const slug = headingSlug(decoded)
  if (!slug) return undefined
  return headings.find((h) => headingSlug(h.text) === slug)
}

/**
 * GitHub's heading slug: lowercase, punctuation dropped, each space a hyphen.
 * Letters and digits of any script survive, so non-Latin headings keep their
 * slugs. Inline Markdown such as `**bold**` or `` `code` `` loses its markers
 * along with the rest of the punctuation.
 */
export function headingSlug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\p{M}\s_-]/gu, '')
    .replace(/\s/g, '-')
}

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}
