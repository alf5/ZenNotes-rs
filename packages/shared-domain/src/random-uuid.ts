/**
 * A random version 4 UUID. `crypto.randomUUID` arrived in WebKit 15.4, and the
 * iPhone app still supports iOS 15.0, whose WebView has `getRandomValues` but
 * not `randomUUID`; the shared code asks here so Cloud sync and backups run on
 * every phone the app installs on.
 */
export function randomUUID(): string {
  const webCrypto = globalThis.crypto
  if (typeof webCrypto.randomUUID === 'function') return webCrypto.randomUUID()
  const bytes = webCrypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6]! & 0x0f) | 0x40
  bytes[8] = (bytes[8]! & 0x3f) | 0x80
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}
