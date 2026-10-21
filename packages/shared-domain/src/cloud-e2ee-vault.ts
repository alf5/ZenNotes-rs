/**
 * The per-vault side of end-to-end encryption: turns the plaintext-shaped
 * items the sync coordinator works with into the wire format of an encrypted
 * vault and back (docs/specs/cloud-e2ee.md). The coordinator never sees a
 * path id or ciphertext, so its change, rename and conflict detection keep
 * comparing plaintext hashes exactly as they do for readable vaults.
 */
import type {
  CloudBackupNoteRestore,
  CloudBackupSnapshotItem,
  CloudSyncChange,
  CloudSyncConflict,
  CloudSyncContent,
  CloudSyncContentMetadata,
  CloudSyncContentReference,
  CloudSyncItemKind,
  CloudSyncManifestItem,
  CloudSyncMutation,
  CloudSyncMutationResponse,
  CloudSyncRevision,
  CloudSyncVault,
  CloudSyncVaultEncryption
} from '@zennotes/bridge-contract/cloud-sync'
import {
  type Bytes,
  CloudE2eeError,
  type CloudE2eeItemMeta,
  CloudPathIndex,
  type Zne1ChunkCipherParams,
  Zne1Purpose,
  beginZne1Decryption,
  beginZne1Encryption,
  decryptItemMeta,
  decryptVaultName,
  decryptZne1,
  decryptZne1Stream,
  encryptItemMeta,
  encryptVaultName,
  encryptZne1,
  encryptZne1Stream,
  fromBase64,
  generateCloudE2eeKey,
  newVaultKeyId,
  sha256Hex,
  toBase64,
  unwrapVaultKey,
  wrapVaultKey
} from './cloud-e2ee'

export const CLOUD_E2EE_HEADER = 'X-ZenNotes-Encryption'
export const CLOUD_E2EE_HEADER_VALUE = 'zne1'
export const ENCRYPTED_VAULT_PLACEHOLDER_NAME = 'Encrypted vault'
const VAULT_KEY_VERSION = 1
const CIPHERTEXT_MEDIA_TYPE = 'application/octet-stream'

/**
 * An encrypted vault had to sync but this device holds no account key. Sync
 * stops here; it never falls back to sending or accepting readable content.
 */
export class CloudE2eeLockedError extends Error {
  readonly code = 'E2EE_LOCKED'

  constructor(message = 'This vault is end-to-end encrypted, so this device needs your passphrase or recovery key before it can sync. Unlock encryption in Settings → Cloud.') {
    super(message)
    this.name = 'CloudE2eeLockedError'
  }
}

/** Where the API client finds the cipher for a vault. */
export interface CloudVaultCipherSource {
  /**
   * The cipher for an encrypted vault, or null for a readable one. Throws
   * CloudE2eeLockedError when the vault is encrypted and this device is locked.
   */
  forVault(vaultId: string): Promise<CloudVaultCipher | null>
}

const textEncoder = new TextEncoder()
const strictUtf8 = new TextDecoder('utf-8', { fatal: true })

function plaintextBytes(content: CloudSyncContent): Uint8Array {
  if (content.encoding === 'utf8') return textEncoder.encode(content.data)
  if (content.encoding === 'base64') return fromBase64(content.data)
  throw new CloudE2eeError('malformed', 'Content is already encrypted.')
}

function requireReadableEncoding(encoding: CloudSyncContentMetadata['encoding']): 'utf8' | 'base64' {
  if (encoding === 'utf8' || encoding === 'base64') return encoding
  throw new CloudE2eeError('malformed', 'Content is already encrypted.')
}

function mismatch(message: string): CloudE2eeError {
  return new CloudE2eeError('tampered', message)
}

export class CloudVaultCipher {
  private constructor(
    readonly vaultId: string,
    readonly keyId: string,
    readonly keyVersion: number,
    private readonly vaultKey: Bytes,
    private readonly paths: CloudPathIndex
  ) {}

  /** Opens an existing encrypted vault with the account key. */
  static async open(
    vault: Pick<CloudSyncVault, 'id' | 'encryption'>,
    accountKey: Uint8Array,
    accountKeyVersion: number
  ): Promise<CloudVaultCipher> {
    const encryption = vault.encryption
    if (!encryption || encryption.scheme !== 'zne1' || !/^[0-9a-f]{32}$/.test(encryption.key_id)) {
      throw new CloudE2eeError('unsupported', 'This vault does not use an encryption scheme ZenNotes knows.')
    }
    if (encryption.key_version !== VAULT_KEY_VERSION) {
      throw new CloudE2eeError('unsupported', `Vault key version ${encryption.key_version} needs a newer ZenNotes.`)
    }
    const vaultKey = await unwrapVaultKey(
      encryption.vault_key_envelope,
      accountKey,
      accountKeyVersion,
      encryption.key_id
    )
    return new CloudVaultCipher(
      vault.id,
      encryption.key_id,
      encryption.key_version,
      vaultKey,
      await CloudPathIndex.forVaultKey(vaultKey)
    )
  }

  /** Makes the `encryption` object for a new vault: a fresh vault key, sealed, and the sealed name. */
  static async prepareVault(
    name: string,
    accountKey: Uint8Array,
    accountKeyVersion: number
  ): Promise<CloudSyncVaultEncryption> {
    const vaultKey = generateCloudE2eeKey()
    const keyId = newVaultKeyId()
    return {
      scheme: 'zne1',
      key_id: keyId,
      key_version: VAULT_KEY_VERSION,
      vault_key_envelope: await wrapVaultKey(vaultKey, accountKey, accountKeyVersion, keyId),
      name: await encryptVaultName(name, { vaultKey, vaultKeyVersion: VAULT_KEY_VERSION })
    }
  }

  async vaultName(encryption: CloudSyncVaultEncryption): Promise<string> {
    return decryptVaultName(encryption.name, { vaultKey: this.vaultKey, vaultKeyVersion: this.keyVersion })
  }

  pathId(path: string): Promise<string> {
    return this.paths.id(path)
  }

  // -------------------------------------------------------------------------
  // Outgoing
  // -------------------------------------------------------------------------

  /** Seals an item's metadata under its path id. */
  async sealMeta(
    path: string,
    kind: CloudSyncItemKind,
    metadata: CloudSyncContentMetadata
  ): Promise<{ pathId: string; meta: string }> {
    const pathId = await this.paths.id(path)
    const meta: CloudE2eeItemMeta = {
      path,
      kind,
      encoding: requireReadableEncoding(metadata.encoding),
      media_type: metadata.media_type,
      sha256: metadata.sha256,
      byte_length: metadata.byte_length
    }
    return {
      pathId,
      meta: await encryptItemMeta(meta, { vaultKey: this.vaultKey, vaultKeyVersion: this.keyVersion, pathId })
    }
  }

  /**
   * Encrypts inline content. The plaintext must still be the bytes its
   * metadata describes, so a file that changed after it was hashed is never
   * sealed under the old hash.
   */
  async sealContent(content: CloudSyncContent): Promise<CloudSyncContent> {
    if (content.data === '' && content.byte_length > 0) {
      throw new CloudE2eeError('malformed', 'Large encrypted files upload through the streamed path.')
    }
    const plaintext = plaintextBytes(content)
    if (plaintext.byteLength !== content.byte_length || (await sha256Hex(plaintext)) !== content.sha256) {
      throw mismatch('The file changed while ZenNotes was preparing its encrypted upload.')
    }
    const sealed = await encryptZne1(plaintext, {
      parentKey: this.vaultKey,
      purpose: Zne1Purpose.content,
      keyVersion: this.keyVersion
    })
    return {
      encoding: 'aes-gcm',
      data: toBase64(sealed),
      sha256: await sha256Hex(sealed),
      byte_length: sealed.byteLength,
      media_type: CIPHERTEXT_MEDIA_TYPE
    }
  }

  /** The wire form of one mutation. Moves need the engine's content hints. */
  async encryptMutation(mutation: CloudSyncMutation): Promise<CloudSyncMutation> {
    const base = {
      operation_id: mutation.operation_id,
      item_id: mutation.item_id,
      base_revision: mutation.base_revision
    }
    if (mutation.type === 'delete') return { ...base, type: 'delete' }
    if (mutation.type === 'move') {
      if (!mutation.kind || !mutation.content_metadata) {
        throw new CloudE2eeError('invalid-metadata', 'An encrypted rename needs the file details the sync engine attaches.')
      }
      const { pathId, meta } = await this.sealMeta(mutation.path, mutation.kind, mutation.content_metadata)
      return { ...base, type: 'move', path: pathId, meta }
    }
    const { pathId, meta } = await this.sealMeta(mutation.path, mutation.kind, mutation.content)
    return {
      ...base,
      type: 'upsert',
      path: pathId,
      kind: 'binary',
      content: await this.sealContent(mutation.content),
      meta
    }
  }

  /** The upload-initiation fields for a streamed upload whose ciphertext is already on disk. */
  async directUploadFields(
    path: string,
    kind: CloudSyncItemKind,
    plaintext: CloudSyncContentMetadata,
    ciphertext: { sha256: string; byte_length: number }
  ): Promise<{ path: string; kind: CloudSyncItemKind; meta: string; content: CloudSyncContentMetadata }> {
    const { pathId, meta } = await this.sealMeta(path, kind, plaintext)
    return {
      path: pathId,
      kind: 'binary',
      meta,
      content: {
        encoding: 'aes-gcm',
        sha256: ciphertext.sha256,
        byte_length: ciphertext.byte_length,
        media_type: CIPHERTEXT_MEDIA_TYPE
      }
    }
  }

  // -------------------------------------------------------------------------
  // Incoming
  // -------------------------------------------------------------------------

  private async openMeta(pathId: string, meta: string | null | undefined): Promise<CloudE2eeItemMeta> {
    if (!meta) {
      throw new CloudE2eeError('invalid-metadata', 'ZenNotes Cloud returned an encrypted item without its metadata.')
    }
    return decryptItemMeta(meta, {
      vaultKey: this.vaultKey,
      vaultKeyVersion: this.keyVersion,
      pathId,
      pathIndex: this.paths
    })
  }

  /** Decrypts inline content and checks it is exactly the file its metadata names. */
  async openContent(content: CloudSyncContent, meta: CloudE2eeItemMeta): Promise<CloudSyncContent> {
    if (content.encoding !== 'aes-gcm') {
      throw mismatch('ZenNotes Cloud returned readable content for an encrypted vault.')
    }
    const sealed = fromBase64(content.data)
    if (sealed.byteLength !== content.byte_length || (await sha256Hex(sealed)) !== content.sha256) {
      throw mismatch('Encrypted content did not match the size and hash ZenNotes Cloud reported.')
    }
    const plaintext = await decryptZne1(sealed, {
      parentKey: this.vaultKey,
      purpose: Zne1Purpose.content,
      keyVersion: this.keyVersion
    })
    if (plaintext.byteLength !== meta.byte_length || (await sha256Hex(plaintext)) !== meta.sha256) {
      throw mismatch('Decrypted content is not the file its metadata names.')
    }
    return {
      encoding: meta.encoding,
      data: meta.encoding === 'utf8' ? strictUtf8.decode(plaintext) : toBase64(plaintext),
      sha256: meta.sha256,
      byte_length: meta.byte_length,
      media_type: meta.media_type
    }
  }

  /**
   * A reference to large content: plaintext metadata for the coordinator, and
   * `encrypted` for the host that downloads and decrypts the stored bytes.
   */
  openReference(reference: CloudSyncContentReference, meta: CloudE2eeItemMeta): CloudSyncContentReference {
    if (reference.encoding !== 'aes-gcm') {
      throw mismatch('ZenNotes Cloud returned a readable reference for an encrypted vault.')
    }
    return {
      item_id: reference.item_id,
      revision: reference.revision,
      encoding: meta.encoding,
      sha256: meta.sha256,
      byte_length: meta.byte_length,
      media_type: meta.media_type,
      encrypted: { sha256: reference.sha256, byte_length: reference.byte_length }
    }
  }

  async openManifestItem(item: CloudSyncManifestItem): Promise<CloudSyncManifestItem> {
    const meta = await this.openMeta(item.path, item.meta)
    return {
      item_id: item.item_id,
      path: meta.path,
      kind: meta.kind,
      revision: item.revision,
      sha256: meta.sha256,
      byte_length: meta.byte_length,
      media_type: meta.media_type,
      ...(item.content ? { content: await this.openContent(item.content, meta) } : {}),
      ...(item.content_ref ? { content_ref: this.openReference(item.content_ref, meta) } : {})
    }
  }

  async openChange(change: CloudSyncChange): Promise<CloudSyncChange> {
    const meta = await this.openMeta(change.path, change.meta)
    const previous =
      change.previous_path !== null && change.previous_meta
        ? (await this.openMeta(change.previous_path, change.previous_meta)).path
        : null
    return {
      sequence: change.sequence,
      item_id: change.item_id,
      type: change.type,
      path: meta.path,
      previous_path: previous,
      revision: change.revision,
      ...(change.content ? { content: await this.openContent(change.content, meta) } : {}),
      ...(change.content_ref ? { content_ref: this.openReference(change.content_ref, meta) } : {})
    }
  }

  async openRevision(revision: CloudSyncRevision): Promise<CloudSyncRevision> {
    const meta = await this.openMeta(revision.path, revision.meta)
    return {
      item_id: revision.item_id,
      revision: revision.revision,
      path: meta.path,
      kind: meta.kind,
      deleted: revision.deleted,
      ...(revision.content !== undefined
        ? { content: revision.content === null ? null : await this.openContent(revision.content, meta) }
        : {}),
      ...(revision.content_ref ? { content_ref: this.openReference(revision.content_ref, meta) } : {})
    }
  }

  async openConflict(conflict: CloudSyncConflict): Promise<CloudSyncConflict> {
    const { current_meta: currentMeta, ...rest } = conflict
    return {
      ...rest,
      current_path:
        conflict.current_path !== null && currentMeta
          ? (await this.openMeta(conflict.current_path, currentMeta)).path
          : null
    }
  }

  async openMutationResponse(response: CloudSyncMutationResponse): Promise<CloudSyncMutationResponse> {
    return {
      ...response,
      conflicts: await Promise.all(response.conflicts.map((conflict) => this.openConflict(conflict)))
    }
  }

  async openBackupItem(item: CloudBackupSnapshotItem): Promise<CloudBackupSnapshotItem> {
    const meta = await this.openMeta(item.path, item.meta)
    return {
      id: item.id,
      item_id: item.item_id,
      path: meta.path,
      kind: meta.kind,
      byte_length: meta.byte_length,
      revision: item.revision,
      content_hash: meta.sha256,
      media_type: meta.media_type
    }
  }

  /**
   * One item of a downloaded backup archive, made readable: the real path and
   * kind from its metadata and the decrypted content, checked against them.
   */
  async openArchiveItem(
    item: Record<string, unknown> & { path: string; meta?: string | null; content: CloudSyncContent }
  ): Promise<Record<string, unknown>> {
    const { meta: sealedMeta, ...rest } = item
    const meta = await this.openMeta(item.path, sealedMeta)
    return { ...rest, path: meta.path, kind: meta.kind, content: await this.openContent(item.content, meta) }
  }

  async openNoteRestore(restore: CloudBackupNoteRestore): Promise<CloudBackupNoteRestore> {
    const { meta, ...rest } = restore
    return { ...rest, path: (await this.openMeta(restore.path, meta)).path }
  }

  // -------------------------------------------------------------------------
  // Streams and native chunk loops
  // -------------------------------------------------------------------------

  encryptStream(source: AsyncIterable<Uint8Array> | Iterable<Uint8Array>): AsyncGenerator<Bytes> {
    return encryptZne1Stream(source, {
      parentKey: this.vaultKey,
      purpose: Zne1Purpose.content,
      keyVersion: this.keyVersion
    })
  }

  decryptStream(source: AsyncIterable<Uint8Array> | Iterable<Uint8Array>): AsyncGenerator<Bytes> {
    return decryptZne1Stream(source, {
      parentKey: this.vaultKey,
      purpose: Zne1Purpose.content,
      keyVersion: this.keyVersion
    })
  }

  /** What a native encryption loop needs for one new file; the vault key stays here. */
  beginContentEncryption(): Promise<Zne1ChunkCipherParams> {
    return beginZne1Encryption({ parentKey: this.vaultKey, purpose: Zne1Purpose.content, keyVersion: this.keyVersion })
  }

  /** Checks a downloaded file's first 74 bytes and returns what its native decryption loop needs. */
  async openContentHeader(header: Uint8Array): Promise<Zne1ChunkCipherParams> {
    const { header: bytes, objectKey, aad } = await beginZne1Decryption(header, {
      parentKey: this.vaultKey,
      purpose: Zne1Purpose.content,
      keyVersion: this.keyVersion
    })
    return { header: bytes, objectKey, aad }
  }
}

/**
 * Removes the sync engine's client-side move hints, for a readable vault whose
 * mutations go to the service unchanged otherwise.
 */
export function withoutClientHints(mutation: CloudSyncMutation): CloudSyncMutation {
  if (mutation.type !== 'move' || (mutation.kind === undefined && mutation.content_metadata === undefined)) {
    return mutation
  }
  const { kind: _kind, content_metadata: _metadata, ...wire } = mutation
  return wire
}
