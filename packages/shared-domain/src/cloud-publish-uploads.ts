import type {
  CloudPublishAssetInput,
  CloudPublishedNoteResult,
  CloudPublishEncodedAsset,
  CloudPublishNoteInput,
  CloudPublishUploadRequest,
  CloudPublishUploadResponse,
  CloudPublishUploadTarget
} from '@zennotes/bridge-contract/cloud-sync'
import type { CloudPublishRequest } from './cloud-sync-api'

/** The service calls a publish needs, from whichever client the platform uses. */
export interface CloudPublishApi {
  publishNote(input: CloudPublishRequest): Promise<CloudPublishedNoteResult>
  updatePublishedNote(shareId: number, input: CloudPublishRequest): Promise<CloudPublishedNoteResult>
  createPublishUpload(body: CloudPublishUploadRequest): Promise<CloudPublishUploadResponse>
  abortPublishUpload(uploadId: string): Promise<void>
}

/** An attachment the platform has looked at: its size and SHA-256, and a handle
 * the platform's own upload understands (a file path, a native URI, bytes). */
export interface DescribedPublishAsset<Handle> {
  byteLength: number
  sha256: string
  handle: Handle
}

/** The file access a publish needs, which only the platform can do. */
export interface CloudPublishAssetPlatform<Handle> {
  describe(asset: CloudPublishAssetInput): Promise<DescribedPublishAsset<Handle>>
  upload(
    target: CloudPublishUploadTarget,
    asset: CloudPublishAssetInput & DescribedPublishAsset<Handle>
  ): Promise<void>
  /** The file's bytes, for a service without staged uploads (the one-request publish). */
  readBase64(asset: CloudPublishAssetInput): Promise<string>
}

export interface CloudPublishUploadOptions {
  /** Republish this share instead of creating one. */
  shareId?: number
  /** Files read or uploaded at once. */
  concurrency?: number
  /** Called after each attachment is in storage. */
  onProgress?(uploaded: number, total: number): void
}

const DEFAULT_CONCURRENCY = 4

/**
 * Publish a note, sending its attachments straight to storage first: the
 * service checks every limit before a byte is sent, then signs one PUT per
 * file, and the publish only names the upload. A service without staged
 * uploads answers 404, and the note goes out the old way, in one request.
 */
export async function publishWithStagedUploads<Handle>(
  api: CloudPublishApi,
  input: CloudPublishNoteInput,
  platform: CloudPublishAssetPlatform<Handle>,
  options: CloudPublishUploadOptions = {}
): Promise<CloudPublishedNoteResult> {
  const { assets = [], ...note } = input
  const concurrency = Math.max(1, options.concurrency ?? DEFAULT_CONCURRENCY)
  const send = (request: CloudPublishRequest): Promise<CloudPublishedNoteResult> =>
    options.shareId === undefined ? api.publishNote(request) : api.updatePublishedNote(options.shareId, request)
  const inOneRequest = async (): Promise<CloudPublishedNoteResult> =>
    await send({ ...note, assets: await encodeAll(assets, platform, concurrency) })

  // Staged uploads carry no logo: a logo change goes the one-request way.
  if (assets.length === 0 || (note.appearance?.logo !== undefined && note.appearance.logo !== null)) {
    return await inOneRequest()
  }

  const described = await mapWithConcurrency(assets, concurrency, async (asset) => ({
    ...asset,
    ...(await platform.describe(asset))
  }))

  let upload: CloudPublishUploadResponse['data']
  try {
    upload = (await api.createPublishUpload({
      ...(options.shareId === undefined ? {} : { share_id: options.shareId }),
      assets: described.map((asset) => ({
        ref: asset.ref,
        name: asset.name,
        mime: asset.mime,
        byte_length: asset.byteLength,
        sha256: asset.sha256
      }))
    })).data
  } catch (error) {
    if (errorStatus(error) === 404) return await inOneRequest()
    throw error
  }

  try {
    const targets = new Map(upload.uploads.map((target) => [target.ref, target]))
    let uploaded = 0
    await mapWithConcurrency(described, concurrency, async (asset) => {
      const target = targets.get(asset.ref)
      if (!target) throw new Error(`ZenNotes Cloud did not prepare an upload for “${asset.name}”.`)
      await platform.upload(target, asset)
      uploaded += 1
      options.onProgress?.(uploaded, described.length)
    })

    return await send({ ...note, upload_id: upload.id, asset_refs: described.map((asset) => asset.ref) })
  } catch (error) {
    // Cancelling an upload a publish already recorded changes nothing, so this
    // is safe even when the publish committed and only its answer was lost.
    await api.abortPublishUpload(upload.id).catch(() => {})
    throw error
  }
}

async function encodeAll<Handle>(
  assets: CloudPublishAssetInput[],
  platform: CloudPublishAssetPlatform<Handle>,
  concurrency: number
): Promise<CloudPublishEncodedAsset[]> {
  return await mapWithConcurrency(assets, concurrency, async (asset) => ({
    ref: asset.ref,
    name: asset.name,
    mime: asset.mime,
    base64: await platform.readBase64(asset)
  }))
}

function errorStatus(error: unknown): number | null {
  if (error && typeof error === 'object' && 'status' in error) {
    const status = (error as { status: unknown }).status
    return typeof status === 'number' ? status : null
  }
  return null
}

/** Map in order, at most `limit` at a time; the first failure rejects. */
export async function mapWithConcurrency<Item, Result>(
  items: readonly Item[],
  limit: number,
  run: (item: Item, index: number) => Promise<Result>
): Promise<Result[]> {
  const results = new Array<Result>(items.length)
  let next = 0
  let failed = false
  const worker = async (): Promise<void> => {
    while (!failed && next < items.length) {
      const index = next
      next += 1
      try {
        results[index] = await run(items[index] as Item, index)
      } catch (error) {
        failed = true
        throw error
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(Math.max(1, limit), items.length) }, worker))
  return results
}
