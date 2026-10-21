/** A user-showable message from a rejected bridge call. Electron wraps main
 *  process rejections as "Error invoking remote method 'x': Error: <real>";
 *  a toast should carry only the real sentence. */
export function humanIpcError(err: unknown, fallback: string): string {
  const raw = err instanceof Error ? err.message : ''
  const message = raw
    .replace(/^Error invoking remote method '[^']*':\s*/, '')
    // Custom error classes arrive by name ("CloudServiceRequestError: ...",
    // "CloudE2eeError: ..."), digits included.
    .replace(/^(?:[A-Z][A-Za-z0-9]*)?Error:\s*/, '')
    .trim()
  return message || fallback
}
