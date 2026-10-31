import type { Plugin } from 'vite'

/**
 * Text in the vendored packages that is wrong for this port. `packages/` stays
 * byte-identical to upstream, so the wording is swapped as the bundle is built,
 * and only here. Each `find` must occur exactly once in its file: when upstream
 * rewords one, the build fails instead of quietly showing the old text again,
 * and src/__tests__/vendored-copy.test.ts says the same without a build.
 */
export const VENDORED_COPY: ReadonlyArray<{ file: string; find: string; replace: string }> = [
  {
    // An Electron flag, which does nothing here.
    file: 'packages/app-core/src/lib/unsplash-key.ts',
    find: ' If a Secret Service keyring is running, launch ZenNotes with --password-store=gnome-libsecret.',
    replace: ' Start a Secret Service keyring, such as KWallet or GNOME Keyring, then restart ZenNotes-rs.'
  },
  {
    // The macOS share extension is not ported (GAP-ANALYSIS.md).
    file: 'packages/app-core/src/lib/whats-new.ts',
    find: "title: 'Share to ZenNotes on macOS',",
    replace: "title: 'Share to ZenNotes: not in this build',"
  },
  {
    file: 'packages/app-core/src/lib/whats-new.ts',
    find: "body: 'Choose Share, ZenNotes in Safari or any app to save a page or the text you selected as a note. Each vault picks where shares land.'",
    replace: "body: 'The original ZenNotes can now save a page or selected text from the macOS Share menu. ZenNotes-rs does not have this yet.'"
  }
]

/** `code` with every swap for `file` made. Throws when one no longer matches once. */
export function applyVendoredCopy(file: string, code: string): string {
  let next = code
  for (const swap of VENDORED_COPY) {
    if (swap.file !== file) continue
    const found = next.split(swap.find).length - 1
    if (found !== 1) {
      throw new Error(
        `vendored-copy: expected one match in ${file}, found ${found}: ${swap.find}\n` +
          'Upstream changed this text. Update or remove the entry in tooling/vite/vendored-copy.ts.'
      )
    }
    next = next.replace(swap.find, () => swap.replace)
  }
  return next
}

export function vendoredCopy(): Plugin {
  const files = [...new Set(VENDORED_COPY.map((swap) => swap.file))]
  return {
    name: 'zennotes-vendored-copy',
    enforce: 'pre',
    transform(code, id) {
      const path = id.split('?')[0].replaceAll('\\', '/')
      const file = files.find((candidate) => path.endsWith(`/${candidate}`))
      return file ? { code: applyVendoredCopy(file, code), map: null } : null
    }
  }
}
