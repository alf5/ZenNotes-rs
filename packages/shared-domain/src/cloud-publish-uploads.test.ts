import { describe, expect, it, vi } from 'vitest'
import type { CloudPublishAssetInput, CloudPublishUploadTarget } from '@zennotes/bridge-contract/cloud-sync'
import {
  mapWithConcurrency,
  publishWithStagedUploads,
  type CloudPublishApi,
  type CloudPublishAssetPlatform
} from './cloud-publish-uploads'

const note = { note_path: 'Notes/Trip.md', title: 'Trip', markdown: '# Trip\n\n![[a.png]] ![[b.png]]' }
const assets: CloudPublishAssetInput[] = [
  { ref: 'a.png', name: 'a.png', mime: 'image/png', path: 'attachements/a.png' },
  { ref: 'b.png', name: 'b.png', mime: 'image/png', path: 'attachements/b.png' }
]
const published = { id: 7, slug: 'trip', url: 'https://zennotes.org/s/trip' }

class StatusError extends Error {
  constructor(readonly status: number) {
    super(`HTTP ${status}`)
  }
}

function setup(overrides: Partial<CloudPublishApi> = {}) {
  const api: CloudPublishApi = {
    publishNote: vi.fn(async () => published),
    updatePublishedNote: vi.fn(async () => published),
    createPublishUpload: vi.fn(async (body) => ({
      data: {
        id: '01upload',
        expires_at: '2026-10-09T20:00:00Z',
        uploads: body.assets.map((asset: { ref: string }): CloudPublishUploadTarget => ({
          ref: asset.ref,
          method: 'PUT',
          url: `https://storage.test/${asset.ref}`,
          headers: { Host: 'storage.test' }
        }))
      }
    })),
    abortPublishUpload: vi.fn(async () => {}),
    ...overrides
  }
  const platform: CloudPublishAssetPlatform<string> = {
    describe: vi.fn(async (asset) => ({ byteLength: asset.ref.length * 100, sha256: `hash-${asset.ref}`, handle: `/vault/${asset.path}` })),
    upload: vi.fn(async () => {}),
    readBase64: vi.fn(async (asset) => `base64-${asset.ref}`)
  }
  return { api, platform }
}

describe('publishWithStagedUploads', () => {
  it('stages each attachment, uploads it to its own target, then publishes naming the upload', async () => {
    const { api, platform } = setup()
    const progress: Array<[number, number]> = []

    const result = await publishWithStagedUploads(api, { ...note, assets }, platform, {
      onProgress: (uploaded, total) => progress.push([uploaded, total])
    })

    expect(result).toEqual(published)
    expect(api.createPublishUpload).toHaveBeenCalledWith({
      assets: [
        { ref: 'a.png', name: 'a.png', mime: 'image/png', byte_length: 500, sha256: 'hash-a.png' },
        { ref: 'b.png', name: 'b.png', mime: 'image/png', byte_length: 500, sha256: 'hash-b.png' }
      ]
    })
    expect(platform.upload).toHaveBeenCalledWith(
      expect.objectContaining({ ref: 'a.png', url: 'https://storage.test/a.png' }),
      expect.objectContaining({ ref: 'a.png', handle: '/vault/attachements/a.png', byteLength: 500 })
    )
    expect(api.publishNote).toHaveBeenCalledWith({ ...note, upload_id: '01upload', asset_refs: ['a.png', 'b.png'] })
    expect(platform.readBase64).not.toHaveBeenCalled()
    expect(api.abortPublishUpload).not.toHaveBeenCalled()
    expect(progress).toEqual([[1, 2], [2, 2]])
  })

  it('republishes a share with an upload made for it', async () => {
    const { api, platform } = setup()

    await publishWithStagedUploads(api, { ...note, assets }, platform, { shareId: 7 })

    expect(api.createPublishUpload).toHaveBeenCalledWith(expect.objectContaining({ share_id: 7 }))
    expect(api.updatePublishedNote).toHaveBeenCalledWith(7, { ...note, upload_id: '01upload', asset_refs: ['a.png', 'b.png'] })
    expect(api.publishNote).not.toHaveBeenCalled()
  })

  it('publishes in one request when the service has no staged uploads', async () => {
    const { api, platform } = setup({ createPublishUpload: vi.fn(async () => { throw new StatusError(404) }) })

    await publishWithStagedUploads(api, { ...note, assets }, platform)

    expect(platform.upload).not.toHaveBeenCalled()
    expect(api.publishNote).toHaveBeenCalledWith({
      ...note,
      assets: [
        { ref: 'a.png', name: 'a.png', mime: 'image/png', base64: 'base64-a.png' },
        { ref: 'b.png', name: 'b.png', mime: 'image/png', base64: 'base64-b.png' }
      ]
    })
  })

  it('reports a refused upload instead of falling back', async () => {
    const refusal = Object.assign(new StatusError(422), { message: 'This note has 51 attached files' })
    const { api, platform } = setup({ createPublishUpload: vi.fn(async () => { throw refusal }) })

    await expect(publishWithStagedUploads(api, { ...note, assets }, platform)).rejects.toBe(refusal)
    expect(api.publishNote).not.toHaveBeenCalled()
  })

  it('cancels the upload when a file or the publish fails', async () => {
    const failingUpload = setup()
    vi.mocked(failingUpload.platform.upload).mockRejectedValueOnce(new Error('offline'))
    await expect(publishWithStagedUploads(failingUpload.api, { ...note, assets }, failingUpload.platform)).rejects.toThrow('offline')
    expect(failingUpload.api.abortPublishUpload).toHaveBeenCalledWith('01upload')
    expect(failingUpload.api.publishNote).not.toHaveBeenCalled()

    const failingPublish = setup({ publishNote: vi.fn(async () => { throw new StatusError(422) }) })
    await expect(publishWithStagedUploads(failingPublish.api, { ...note, assets }, failingPublish.platform)).rejects.toBeInstanceOf(StatusError)
    expect(failingPublish.api.abortPublishUpload).toHaveBeenCalledWith('01upload')
  })

  it('sends a note without attachments, or one that changes the logo, in one request', async () => {
    const plain = setup()
    await publishWithStagedUploads(plain.api, note, plain.platform)
    expect(plain.api.createPublishUpload).not.toHaveBeenCalled()
    expect(plain.api.publishNote).toHaveBeenCalledWith({ ...note, assets: [] })

    const logo = setup()
    const appearance = { theme: 'system', logo: { ref: 'brand-logo', name: 'logo.png', mime: 'image/png', base64: 'AQID' } }
    await publishWithStagedUploads(logo.api, { ...note, assets, appearance }, logo.platform)
    expect(logo.api.createPublishUpload).not.toHaveBeenCalled()
    expect(logo.platform.readBase64).toHaveBeenCalledTimes(2)
  })
})

describe('mapWithConcurrency', () => {
  it('keeps order and never runs more than the limit at once', async () => {
    let running = 0
    let peak = 0
    const results = await mapWithConcurrency([1, 2, 3, 4, 5, 6, 7], 3, async (value) => {
      running += 1
      peak = Math.max(peak, running)
      await new Promise((resolve) => setTimeout(resolve, 5))
      running -= 1
      return value * 10
    })

    expect(results).toEqual([10, 20, 30, 40, 50, 60, 70])
    expect(peak).toBe(3)
  })
})
