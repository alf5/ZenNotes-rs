import { describe, expect, it } from 'vitest'
import { assetMoveTargetDir, buildMoveAssetPrompt, validateAssetMoveTarget } from './move-asset'

describe('validateAssetMoveTarget', () => {
  it('refuses a whole .. segment, with either separator', () => {
    expect(validateAssetMoveTarget('..')).toBe('Path cannot contain a .. folder')
    expect(validateAssetMoveTarget('media/../secrets')).toBe('Path cannot contain a .. folder')
    expect(validateAssetMoveTarget(' ../media ')).toBe('Path cannot contain a .. folder')
    expect(validateAssetMoveTarget('media\\..\\secrets')).toBe('Path cannot contain a .. folder')
  })

  it('accepts folder names that only contain two dots', () => {
    expect(validateAssetMoveTarget('Notes..old')).toBeNull()
    expect(validateAssetMoveTarget('media/v1..2/shots')).toBeNull()
    expect(validateAssetMoveTarget('...')).toBeNull()
    expect(validateAssetMoveTarget('')).toBeNull()
  })

  it('keeps assets out of the internal folder', () => {
    expect(validateAssetMoveTarget('.zennotes/x')).toBe(
      'Cannot move assets into internal ZenNotes files'
    )
    expect(validateAssetMoveTarget('media/.zennotes')).toBe(
      'Cannot move assets into internal ZenNotes files'
    )
    expect(validateAssetMoveTarget('media/.zennotes-old')).toBeNull()
  })
})

describe('assetMoveTargetDir', () => {
  it('sends an empty answer to the assets folder, where every host puts it', () => {
    expect(assetMoveTargetDir('')).toBe('assets')
    expect(assetMoveTargetDir('  ')).toBe('assets')
    expect(assetMoveTargetDir('/')).toBe('assets')
  })

  it('keeps a typed folder, without its outer slashes', () => {
    expect(assetMoveTargetDir('media/screenshots')).toBe('media/screenshots')
    expect(assetMoveTargetDir(' /media/screenshots/ ')).toBe('media/screenshots')
  })
})

describe('buildMoveAssetPrompt', () => {
  it('opens on the current folder and says where an empty answer goes', () => {
    const prompt = buildMoveAssetPrompt('media')
    expect(prompt.initialValue).toBe('media')
    expect(prompt.allowEmptySubmit).toBe(true)
    expect(prompt.description).toContain('Leave empty to move it to the assets/ folder.')
    expect(prompt.description).not.toContain('vault root')
    expect(prompt.validate?.('a/../b')).toBe('Path cannot contain a .. folder')
  })
})
