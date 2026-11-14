/**
 * Clip links, `zennotes-rs://new` and `zennotes-rs://daily`: the port's copy of
 * upstream's apps/desktop/src/main/clip-links.ts (v2.85, #620). The contract
 * is upstream's docs/web-clipper-link.md under the port's own scheme, so the
 * vendored parser (`@zennotes/shared-domain/clip-link`) reads the link once
 * the scheme is put back to `zennotes:`.
 *
 * Upstream runs this in the main process and hands a clip to the window that
 * shows the vault it names, which writes it through its store (an open
 * note's unsaved edits live there). Here Rust passes the raw link to the main
 * window (src-tauri/src/clip_links.rs) and this module, running in that
 * window, does the rest. The port opens one workspace per run, so the window
 * that shows the clip's vault is this one or none:
 *
 *  - a clip for the open workspace goes to app-core's clip-apply, exactly as
 *    upstream's `sendClip`;
 *  - a clip naming another vault is refused with a sentence saying which, the
 *    way upstream refuses one for a server that is not connected. Upstream
 *    writes such a clip from main into a vault no window shows; the port
 *    keeps one vault open at a time and does not write into another.
 *
 * The clipboard never reaches this webview unchecked: Rust reads it and hands
 * back only text whose SHA-256 is the one the link named.
 */
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { ClipApplyRequest, ClipNotice } from '@zennotes/bridge-contract/clip'
import type { LocalVaultEntry, RemoteWorkspaceInfo, RemoteWorkspaceProfile, VaultInfo } from '@zennotes/bridge-contract/ipc'
import { parseClipLink, type ClipLinkRejection, type ClipLinkRequest } from '@zennotes/shared-domain/clip-link'
import { invoke } from './ipc'

/** Upstream's REJECTION_MESSAGES, verbatim. */
const REJECTION_MESSAGES: Record<ClipLinkRejection, string> = {
  'link-too-long': 'That clip is too long for a link. Clip it again from the web clipper.',
  'content-too-long': 'That clip is too long for a link. Clip it again from the web clipper.',
  'bad-file': 'That clip names a place in the vault ZenNotes cannot write to.',
  'bad-vault': 'That clip names a vault ZenNotes cannot read.',
  'bad-encoding': 'That clip link is malformed, so nothing was saved.',
  'duplicate-param': 'That clip link is malformed, so nothing was saved.',
  'bad-hash': 'That clip link is malformed, so nothing was saved.',
  'two-bodies': 'That clip link is malformed, so nothing was saved.',
  'empty-body': 'That clip was empty, so nothing was saved.',
  'append-and-prepend': 'That clip link is malformed, so nothing was saved.',
  'missing-file': 'That clip names no note to add to.',
  'nothing-to-insert': 'That clip had nothing to add.'
}

const NOT_ARRIVED = 'The clip did not arrive (the clipboard changed). Clip it again.'
const TOO_LARGE = 'That clip is larger than 8 MB, so nothing was saved.'
/** app-core's CLIP_MESSAGES.failed (lib/clip-plan, not an exported entry). */
const FAILED = 'The clip could not be saved.'

/** The port's scheme read as upstream's, for the vendored parser. */
export function asUpstreamClipLink(rawUrl: string): string {
  return rawUrl.trim().replace(/^zennotes-rs:/iu, 'zennotes:')
}

/** A vault a `vault=` name can pick: a local vault, or a saved server. */
export type ClipVaultChoice =
  | { kind: 'local'; root: string; name: string }
  | { kind: 'remote'; profileId: string; name: string }

/** What the open workspace is, for picking the clip's target. */
export interface ClipWorkspace {
  /** The open local vault, or null in remote mode or with none open. */
  localRoot: string | null
  /** The connected server's saved profile, in remote mode. */
  remoteProfileId: string | null
  remote: boolean
}

function sameName(a: string, b: string): boolean {
  return a.normalize('NFC').toLowerCase() === b.normalize('NFC').toLowerCase()
}

function basename(root: string): string {
  const parts = root.split(/[\\/]+/u).filter(Boolean)
  return parts[parts.length - 1] ?? root
}

function samePath(a: string, b: string): boolean {
  const clean = (p: string): string => p.replace(/[\\/]+$/u, '')
  return clean(a) === clean(b)
}

/** Port of `matchClipVaults`: a vault's name, a local vault's folder name, or
 *  a server profile's name. Never a path. */
export function matchClipVaults(name: string, choices: ClipVaultChoice[]): ClipVaultChoice[] {
  return choices.filter((choice) => {
    if (sameName(choice.name, name)) return true
    return choice.kind === 'local' && sameName(basename(choice.root), name)
  })
}

export type ClipDecision =
  | { kind: 'refused'; message: string }
  | { kind: 'apply'; request: ClipApplyRequest }

/**
 * Port of `run`'s routing, with the clip's text already in hand: which vault
 * the link names, and whether the open workspace is it. Pure, for tests.
 */
export function decideClip(
  request: ClipLinkRequest,
  text: string | null,
  workspace: ClipWorkspace,
  choices: ClipVaultChoice[]
): ClipDecision {
  let target: ClipVaultChoice | 'workspace'
  if (request.vault) {
    const matches = matchClipVaults(request.vault, choices)
    if (matches.length === 0) {
      return {
        kind: 'refused',
        message: `No vault is named "${request.vault}". Check the vault name in the web clipper.`
      }
    }
    if (matches.length > 1) {
      return {
        kind: 'refused',
        message: `More than one vault is named "${request.vault}". Use its folder name instead.`
      }
    }
    target = matches[0]!
  } else {
    target = 'workspace'
  }

  const isOpen =
    target === 'workspace'
      ? workspace.remote || workspace.localRoot !== null
      : target.kind === 'local'
        ? !workspace.remote && workspace.localRoot !== null && samePath(workspace.localRoot, target.root)
        : workspace.remote && workspace.remoteProfileId === target.profileId
  if (!isOpen) {
    if (target === 'workspace') {
      return { kind: 'refused', message: 'Open a vault in ZenNotes-rs, then clip again.' }
    }
    return {
      kind: 'refused',
      message:
        target.kind === 'remote'
          ? `Connect to "${target.name}" in ZenNotes-rs, then clip again.`
          : `That clip is for "${target.name}". Open that vault in ZenNotes-rs, then clip again.`
    }
  }

  return {
    kind: 'apply',
    request: {
      vaultRoot: workspace.remote ? null : workspace.localRoot,
      action: request.action,
      folder: request.action === 'new' ? request.folder : '',
      name: request.action === 'new' ? request.name : null,
      text,
      position: request.position,
      silent: request.silent
    }
  }
}

type ClipboardCheck = { kind: 'verified'; text: string } | { kind: 'notArrived' } | { kind: 'tooLarge' }

/** The text a request carries, or a sentence saying why there is none. */
async function clipText(request: ClipLinkRequest): Promise<{ text: string | null } | { problem: string }> {
  if (!request.body) return { text: null }
  if ('content' in request.body) return { text: request.body.content }
  const check = await invoke<ClipboardCheck>('clip_verified_clipboard_text', {
    sha256: request.body.clipboard
  }).catch((): ClipboardCheck => ({ kind: 'notArrived' }))
  if (check.kind === 'verified') return { text: check.text }
  return { problem: check.kind === 'tooLarge' ? TOO_LARGE : NOT_ARRIVED }
}

async function currentWorkspace(): Promise<ClipWorkspace> {
  const [info, vault] = await Promise.all([
    invoke<RemoteWorkspaceInfo | null>('workspace_get_info').catch(() => null),
    invoke<VaultInfo | null>('vault_get_current').catch(() => null)
  ])
  const remote = info?.mode === 'remote'
  return {
    remote,
    localRoot: remote ? null : (vault?.root ?? null),
    remoteProfileId: remote ? (info?.profileId ?? null) : null
  }
}

async function vaultChoices(): Promise<ClipVaultChoice[]> {
  const [locals, profiles] = await Promise.all([
    invoke<LocalVaultEntry[]>('vault_list_local').catch((): LocalVaultEntry[] => []),
    invoke<RemoteWorkspaceProfile[]>('workspace_list_remote_profiles').catch((): RemoteWorkspaceProfile[] => [])
  ])
  return [
    ...locals.map((entry): ClipVaultChoice => ({ kind: 'local', root: entry.root, name: entry.name })),
    ...profiles.map((profile): ClipVaultChoice => ({ kind: 'remote', profileId: profile.id, name: profile.name }))
  ]
}

const clipListeners = new Set<(request: ClipApplyRequest) => void>()
const noticeListeners = new Set<(notice: ClipNotice) => void>()
/** Clips that arrived before app-core subscribed (a cold start). */
const waitingClips: ClipApplyRequest[] = []
const waitingNotices: ClipNotice[] = []

function notify(message: string): void {
  const notice: ClipNotice = { kind: 'error', message }
  if (noticeListeners.size === 0) waitingNotices.push(notice)
  for (const cb of noticeListeners) cb(notice)
}

function hand(request: ClipApplyRequest): void {
  if (clipListeners.size === 0) waitingClips.push(request)
  for (const cb of clipListeners) cb(request)
}

async function raiseWindow(): Promise<void> {
  await invoke('clip_raise_window').catch(() => {})
}

/** One link, start to finish. Never throws. */
export async function runClipLink(rawUrl: string): Promise<void> {
  const parsed = parseClipLink(asUpstreamClipLink(rawUrl))
  if (!parsed) return
  if (!parsed.ok) {
    console.log(`[clip] refused: ${parsed.reason}`)
    notify(REJECTION_MESSAGES[parsed.reason])
    return
  }
  const { request } = parsed
  const text = await clipText(request)
  if ('problem' in text) {
    console.log('[clip] clipboard did not match')
    notify(text.problem)
    return
  }
  const [workspace, choices] = await Promise.all([currentWorkspace(), vaultChoices()])
  const decision = decideClip(request, text.text, workspace, choices)
  if (decision.kind === 'refused') {
    notify(decision.message)
    return
  }
  hand(decision.request)
  if (!request.silent) await raiseWindow()
}

let chain: Promise<void> = Promise.resolve()
let started = false

/** Take clip links in the main window, one at a time in arrival order. */
export function startClipLinks(): void {
  if (started || getCurrentWindow().label !== 'main') return
  started = true
  void listen<string>('app://clip-link', (event) => {
    if (typeof event.payload !== 'string') return
    const raw = event.payload
    chain = chain
      .then(() => runClipLink(raw))
      .catch((err) => {
        console.log(`[clip] failed: ${String(err)}`)
        notify(FAILED)
      })
  })
}

export function onClipRequested(cb: (request: ClipApplyRequest) => void): () => void {
  clipListeners.add(cb)
  for (const request of waitingClips.splice(0)) cb(request)
  return () => {
    clipListeners.delete(cb)
  }
}

export function onClipNotice(cb: (notice: ClipNotice) => void): () => void {
  noticeListeners.add(cb)
  for (const notice of waitingNotices.splice(0)) cb(notice)
  return () => {
    noticeListeners.delete(cb)
  }
}
