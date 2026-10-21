import type {
  CloudSyncContent,
  CloudSyncContentMetadata,
  CloudSyncContentReference,
  CloudSyncDownloadInstruction,
  CloudSyncEncryptedContent
} from '@zennotes/bridge-contract/cloud-sync'
import { cloudProtocolError } from './cloud-error-copy'

/** Host-only token. Its native handle is never serialized or accepted from HTTP. */
export interface CloudSyncStagedFile {
  readonly source: 'file'
  readonly reference: Readonly<CloudSyncContentReference>
  readonly preview?: Readonly<CloudSyncContent>
}

/** Decrypts an encrypted vault's stored bytes; a CloudVaultCipher is one. */
export interface CloudContentDecryptor {
  decryptStream(source: AsyncIterable<Uint8Array> | Iterable<Uint8Array>): AsyncGenerator<Uint8Array>
  /** For native loops: checks a file's first 74 bytes and returns what decrypting it needs. */
  openContentHeader(header: Uint8Array): Promise<{ header: Uint8Array; objectKey: Uint8Array; aad: Uint8Array }>
}

export interface CloudSyncDownloadSource {
  reference: CloudSyncContentReference
  /** Obtain just in time; refresh only the same immutable reference. */
  getInstruction(): Promise<CloudSyncDownloadInstruction>
  signal?: AbortSignal
  /** Optional UTF-8 preview; never bridge more than this many bytes. */
  previewLimitBytes: number
  allowInsecureLoopback?: boolean
  /**
   * Present exactly when the reference is `encrypted`: the host checks the
   * downloaded ciphertext against `reference.encrypted`, decrypts it with this,
   * then checks the plaintext against the reference as for any download.
   */
  decryptor?: CloudContentDecryptor
}

export interface CloudSyncStagedConflict {
  expected_path: string | null
  expected_sha256: string | null
  cloud_path: string
  file: CloudSyncStagedFile
  /** Copy the original local file here before applying Cloud. */
  keep_both_path?: string
}

const stagedFiles = new WeakMap<
  CloudSyncStagedFile,
  { handle: unknown; discard(): Promise<void> }
>()
const metadataKeys = ['encoding', 'sha256', 'byte_length', 'media_type']

export function validateCloudSyncContentReference(
  value: unknown,
  expected?: {
    item_id: string
    revision: number
    sha256?: string
    byte_length?: number
    media_type?: string
    encoding?: CloudSyncContent['encoding']
  }
): CloudSyncContentReference {
  const ref = record(value)
  if (
    !ref ||
    !onlyKeys(ref, [...metadataKeys, 'item_id', 'revision', 'encrypted']) ||
    typeof ref.item_id !== 'string' ||
    !ref.item_id ||
    !Number.isSafeInteger(ref.revision) ||
    (ref.revision as number) < 1
  )
    invalid()
  const metadata = validatedMetadata(ref)
  const encrypted = ref.encrypted === undefined ? undefined : validatedEncrypted(ref.encrypted)
  if (
    expected &&
    (ref.item_id !== expected.item_id ||
      (ref.revision as number) > expected.revision ||
      !sameMetadata(metadata, expected))
  )
    invalid()
  return {
    item_id: ref.item_id as string,
    revision: ref.revision as number,
    ...metadata,
    ...(encrypted ? { encrypted } : {})
  }
}

/**
 * An encrypted vault's reference describes the plaintext for the coordinator
 * and, in `encrypted`, the stored ciphertext the download returns. The
 * smallest ZNE1 object is 90 bytes (an empty file).
 */
function validatedEncrypted(value: unknown): CloudSyncEncryptedContent {
  const encrypted = record(value)
  if (
    !encrypted ||
    !onlyKeys(encrypted, ['sha256', 'byte_length']) ||
    typeof encrypted.sha256 !== 'string' ||
    !/^[a-f0-9]{64}$/.test(encrypted.sha256) ||
    !Number.isSafeInteger(encrypted.byte_length) ||
    (encrypted.byte_length as number) < 90
  )
    invalid()
  return { sha256: encrypted.sha256 as string, byte_length: encrypted.byte_length as number }
}

export function validateCloudSyncDownloadInstruction(
  value: unknown,
  reference: CloudSyncContentReference,
  allowInsecureLoopback = false
): CloudSyncDownloadInstruction {
  const ref = validateCloudSyncContentReference(reference)
  const data = record(record(value)?.data)
  const content = record(data?.content)
  const download = record(data?.download)
  const headers = record(download?.headers)
  if (
    !data ||
    !onlyKeys(data, ['item_id', 'revision', 'content', 'download']) ||
    data.item_id !== ref.item_id ||
    data.revision !== ref.revision ||
    !content ||
    !onlyKeys(content, metadataKeys) ||
    !instructionContentMatches(content, ref) ||
    !download ||
    !onlyKeys(download, ['method', 'url', 'headers', 'expires_at']) ||
    download.method !== 'GET' ||
    typeof download.url !== 'string' ||
    typeof download.expires_at !== 'string' ||
    !Number.isFinite(Date.parse(download.expires_at)) ||
    !headers ||
    Object.entries(headers).some(
      ([key, value]) =>
        typeof value !== 'string' || /^(authorization|proxy-authorization|cookie|host)$/i.test(key)
    )
  )
    invalid()
  validateCloudSyncDownloadUrl(download.url as string, allowInsecureLoopback)
  return {
    item_id: ref.item_id,
    revision: ref.revision,
    content: ref.encrypted ? encryptedInstructionContent(content) : validatedMetadata(content),
    download: {
      method: 'GET',
      url: download.url as string,
      headers: { ...headers } as Record<string, string>,
      expires_at: download.expires_at as string
    }
  }
}

/**
 * A readable reference's download must match its metadata exactly. An
 * encrypted reference's download is the ciphertext, so it must be `aes-gcm`
 * and match `encrypted`; nothing else may arrive as encrypted bytes.
 */
function instructionContentMatches(content: Record<string, unknown>, ref: CloudSyncContentReference): boolean {
  if (!ref.encrypted) return sameMetadata(validatedMetadata(content), ref)
  const sealed = encryptedInstructionContent(content)
  return sealed.sha256 === ref.encrypted.sha256 && sealed.byte_length === ref.encrypted.byte_length
}

function encryptedInstructionContent(content: Record<string, unknown>): CloudSyncContentMetadata {
  if (
    content.encoding !== 'aes-gcm' ||
    typeof content.sha256 !== 'string' ||
    !/^[a-f0-9]{64}$/.test(content.sha256) ||
    !Number.isSafeInteger(content.byte_length) ||
    typeof content.media_type !== 'string' ||
    !content.media_type
  )
    invalid()
  return {
    encoding: 'aes-gcm',
    sha256: content.sha256 as string,
    byte_length: content.byte_length as number,
    media_type: content.media_type as string
  }
}

export function validateCloudSyncDownloadUrl(value: string, allowInsecureLoopback = false): void {
  const url = new URL(value)
  const host = url.hostname.replace(/^\[|\]$/g, '').toLowerCase()
  const loopback = host === 'localhost' || host === '::1' || /^127\.\d+\.\d+\.\d+$/.test(host)
  if (
    url.username ||
    url.password ||
    url.hash ||
    (url.protocol !== 'https:' && !(allowInsecureLoopback && loopback && url.protocol === 'http:'))
  )
    invalid()
}

/** Call only after the native downloader has verified the on-disk length and
 * SHA-256. The coordinator validates the token/reference again before apply. */
export function registerCloudSyncStagedFile(
  reference: CloudSyncContentReference,
  handle: unknown,
  discard: () => Promise<void>,
  preview?: CloudSyncContent
): CloudSyncStagedFile {
  if (typeof discard !== 'function') throw cloudProtocolError('Cloud staging requires a cleanup callback.')
  const file: CloudSyncStagedFile = Object.freeze({
    source: 'file',
    reference: Object.freeze(validateCloudSyncContentReference(reference)),
    ...(preview ? { preview: Object.freeze({ ...preview }) } : {})
  })
  stagedFiles.set(file, { handle, discard })
  return file
}

export function cloudSyncStagedHandle<Handle = unknown>(
  file: CloudSyncStagedFile,
  expected?: CloudSyncContentReference
): Handle {
  const stored = stagedFiles.get(file)
  if (!stored) throw cloudProtocolError('Untrusted or released Cloud staging handle.')
  if (expected) validateCloudSyncContentReference(expected)
  if (
    expected &&
    (file.reference.revision !== expected.revision ||
      file.reference.item_id !== expected.item_id ||
      !sameMetadata(file.reference, expected))
  )
    invalid()
  return stored.handle as Handle
}

export async function releaseCloudSyncStagedFile(file: CloudSyncStagedFile): Promise<void> {
  const stored = stagedFiles.get(file)
  if (!stored) return
  stagedFiles.delete(file)
  await stored.discard()
}

export function throwIfCloudSyncCancelled(signal?: AbortSignal): void {
  if (signal?.aborted)
    throw signal.reason ?? new DOMException('Cloud download cancelled.', 'AbortError')
}

function validatedMetadata(value: Record<string, unknown>): CloudSyncContentMetadata {
  if (
    !['utf8', 'base64'].includes(value.encoding as string) ||
    typeof value.sha256 !== 'string' ||
    !/^[a-f0-9]{64}$/.test(value.sha256) ||
    !Number.isSafeInteger(value.byte_length) ||
    (value.byte_length as number) < 0 ||
    typeof value.media_type !== 'string' ||
    !value.media_type
  )
    invalid()
  return {
    encoding: value.encoding as 'utf8' | 'base64',
    sha256: value.sha256 as string,
    byte_length: value.byte_length as number,
    media_type: value.media_type as string
  }
}

function sameMetadata(
  actual: CloudSyncContentMetadata,
  expected: Partial<CloudSyncContentMetadata>
): boolean {
  return metadataKeys.every(
    (key) =>
      expected[key as keyof CloudSyncContentMetadata] === undefined ||
      actual[key as keyof CloudSyncContentMetadata] ===
        expected[key as keyof CloudSyncContentMetadata]
  )
}

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function onlyKeys(value: Record<string, unknown>, keys: string[]): boolean {
  return Object.keys(value).every((key) => keys.includes(key))
}

function invalid(): never {
  throw cloudProtocolError('Invalid Cloud content reference or download instruction.')
}
