import type { PromptOptions } from '../components/PromptModal'

/**
 * Where an asset goes when the Move asset field is left empty. Every host files
 * it there rather than loose at the vault root: the desktop vault, the server
 * (`cleanAssetTargetDir` in both) and the phone shells' local vaults.
 */
export const DEFAULT_ASSET_FOLDER = 'assets'

/**
 * Why `value` cannot be a Move asset destination, or null. Only a whole `..`
 * segment is refused, never a name that merely contains two dots, so
 * `Notes..old` and `v1..2` stay valid folders (the substring check this
 * replaces refused them, the same bug class as zennotesandroid#130). The
 * hosts resolve the path against the vault and refuse an escape on their own;
 * this check is for the message.
 */
export function validateAssetMoveTarget(value: string): string | null {
  const segments = value.trim().split(/[\\/]/)
  if (segments.includes('..')) return 'Path cannot contain a .. folder'
  if (segments.includes('.zennotes')) return 'Cannot move assets into internal ZenNotes files'
  return null
}

/** The folder an asset moves to for what the prompt returned: the typed path,
 *  or the assets folder for an empty one. */
export function assetMoveTargetDir(value: string): string {
  return value.trim().replace(/^\/+|\/+$/g, '') || DEFAULT_ASSET_FOLDER
}

/** The Move asset prompt shared by the sidebar tree, a folder's note list and
 *  a rendered image's menu. It opens on the asset's current folder. */
export function buildMoveAssetPrompt(currentDir: string): PromptOptions {
  return {
    title: 'Move asset',
    description: `Enter a vault-relative folder path. Leave empty to move it to the ${DEFAULT_ASSET_FOLDER}/ folder.`,
    initialValue: currentDir,
    placeholder: 'media/screenshots',
    okLabel: 'Move',
    allowEmptySubmit: true,
    validate: validateAssetMoveTarget
  }
}
