/**
 * The import members of `window.zen` (upstream's preload: OBSIDIAN_IMPORT_*,
 * LOGSEQ_IMPORT_*, NOTION_IMPORT_*). The controllers, and the vendored
 * conversion code they pull in, load on first use; progress listeners are
 * plain event subscriptions.
 *
 * The parts, each the port's copy of an upstream main-process file:
 * obsidian-import.ts, logseq-import.ts, notion-import.ts (the controllers),
 * *-import-run.ts (the runs), with the host in src-tauri/src/imports.
 */
import type { ZenBridge } from '@zennotes/bridge-contract/bridge'
import { onProgress } from './host'

const obsidian = async () => (await import('./obsidian-import')).obsidianImport
const logseq = async () => (await import('./logseq-import')).logseqImport
const notion = async () => (await import('./notion-import')).notionImport

type ImportMembers = Required<
  Pick<
    ZenBridge,
    | 'obsidianImportStatus'
    | 'obsidianImportPick'
    | 'obsidianImportStart'
    | 'obsidianImportStop'
    | 'obsidianImportDismiss'
    | 'onObsidianImportProgress'
    | 'logseqImportStatus'
    | 'logseqImportPick'
    | 'logseqImportStart'
    | 'logseqImportStop'
    | 'logseqImportDismiss'
    | 'onLogseqImportProgress'
    | 'notionImportStatus'
    | 'notionImportSaveToken'
    | 'notionImportSignIn'
    | 'notionImportCancelSignIn'
    | 'notionImportSignOut'
    | 'notionImportTree'
    | 'notionImportTreeChildren'
    | 'notionImportStart'
    | 'notionImportStop'
    | 'notionImportDiscard'
    | 'onNotionImportProgress'
  >
>

export const importMembers: ImportMembers = {
  obsidianImportStatus: async () => (await obsidian()).status(),
  obsidianImportPick: async () => (await obsidian()).pick(),
  obsidianImportStart: async (options) => (await obsidian()).start(options ?? {}),
  obsidianImportStop: async () => (await obsidian()).stop(),
  obsidianImportDismiss: async () => (await obsidian()).dismiss(),
  onObsidianImportProgress: (cb) => onProgress('obsidian', cb),

  logseqImportStatus: async () => (await logseq()).status(),
  logseqImportPick: async () => (await logseq()).pick(),
  logseqImportStart: async (options) => (await logseq()).start(options ?? {}),
  logseqImportStop: async () => (await logseq()).stop(),
  logseqImportDismiss: async () => (await logseq()).dismiss(),
  onLogseqImportProgress: (cb) => onProgress('logseq', cb),

  notionImportStatus: async () => (await notion()).status(),
  notionImportSaveToken: async (token) => (await notion()).saveToken(token),
  notionImportSignIn: async () => (await notion()).signIn(),
  notionImportCancelSignIn: async () => (await notion()).cancelSignIn(),
  notionImportSignOut: async () => (await notion()).signOut(),
  notionImportTree: async () => (await notion()).tree(),
  notionImportTreeChildren: async (pageId) => (await notion()).treeChildren(pageId),
  notionImportStart: async (options) => (await notion()).start(options ?? {}),
  notionImportStop: async () => (await notion()).stop(),
  notionImportDiscard: async () => (await notion()).discard(),
  onNotionImportProgress: (cb) => onProgress('notion', cb)
}
