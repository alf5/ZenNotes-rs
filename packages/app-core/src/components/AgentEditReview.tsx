import { useCallback, useLayoutEffect, useRef, useState, type MutableRefObject } from 'react'
import type { Compartment, Extension } from '@codemirror/state'
import type { EditorView } from '@codemirror/view'
import { agentEditBaseline, latestAgentEdit, reviewedAgentEdit } from '@shared/agent-edits'
import type { AgentEditRecord } from '@shared/ipc'
import { loadAgentReview } from '../lib/agent-review-runtime'
export { REVIEW_AGENT_EDITS_EVENT } from '../lib/agent-review-runtime'
import { getKeymapBinding, isUnboundBinding, type KeymapId } from '../lib/keymaps'
import { useToastStore } from '../lib/toast'
import { toCodeMirrorKey } from '../lib/vim-half-page-keymap'
import { useStore } from '../store'
import { Button } from './ui/Button'

/**
 * Reviewing an assistant's edits to a note (docs/ideas/agent-edit-review.md):
 * the banner that says a note has changes to review, the bar shown while
 * reviewing, and the hook that puts a pane's editor in and out of review mode.
 */

function timeAgo(at: number, now: number): string {
  const minutes = Math.round((now - at) / 60000)
  if (minutes < 1) return 'just now'
  if (minutes === 1) return 'a minute ago'
  if (minutes < 60) return `${minutes} minutes ago`
  const hours = Math.round(minutes / 60)
  if (hours === 1) return 'an hour ago'
  if (hours < 24) return `${hours} hours ago`
  const days = Math.round(hours / 24)
  if (days === 1) return 'yesterday'
  if (days < 7) return `${days} days ago`
  return `on ${new Date(at).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}`
}

export interface AgentReviewSession {
  path: string
  agent: string
  remaining: number
}

export function AgentEditNoteBanner({
  path,
  reviewing,
  onReview
}: {
  path: string
  reviewing: boolean
  onReview: () => void
}): JSX.Element | null {
  const summary = useStore((s) => s.agentEdits[path] ?? null)
  if (!summary || reviewing) return null
  return (
    <div
      data-agent-edit-banner=""
      className="flex shrink-0 items-center gap-3 border-b border-accent/25 bg-accent/10 px-4 py-1.5 text-sm"
    >
      <p role="status" className="min-w-0 flex-1 text-ink-800">
        {summary.agent} changed this note {timeAgo(summary.at, Date.now())}.
      </p>
      <Button
        data-agent-edit-review=""
        variant="secondary"
        className="shrink-0"
        // Keep the keyboard in the editor: the review happens there.
        onMouseDown={(event) => event.preventDefault()}
        onClick={onReview}
      >
        Review changes
      </Button>
    </div>
  )
}

export function AgentReviewBar({
  session,
  onAcceptAll,
  onRejectAll,
  onDone
}: {
  session: AgentReviewSession
  onAcceptAll: () => void
  onRejectAll: () => void
  onDone: () => void
}): JSX.Element {
  const keep = (event: React.MouseEvent): void => event.preventDefault()
  return (
    <div
      data-agent-review-bar=""
      className="flex shrink-0 items-center gap-2 border-b border-accent/25 bg-accent/10 px-4 py-1.5 text-sm"
    >
      <p role="status" className="min-w-0 flex-1 text-ink-800">
        Reviewing {session.agent}’s changes:{' '}
        {session.remaining === 1 ? '1 left' : `${session.remaining} left`}
      </p>
      <Button variant="secondary" className="shrink-0" onMouseDown={keep} onClick={onAcceptAll}>
        Accept all
      </Button>
      <Button variant="secondary" className="shrink-0" onMouseDown={keep} onClick={onRejectAll}>
        Reject all
      </Button>
      <Button data-agent-review-done="" variant="primary" className="shrink-0" onMouseDown={keep} onClick={onDone}>
        Done
      </Button>
    </div>
  )
}

function keySpecs(id: KeymapId): string[] {
  const binding = getKeymapBinding(useStore.getState().keymapOverrides, id)
  return isUnboundBinding(binding) ? [] : [toCodeMirrorKey(binding)]
}

/** A Vim sequence binding ("d p") as the keys it is typed with. */
function vimSequence(id: KeymapId): string[][] {
  const binding = getKeymapBinding(useStore.getState().keymapOverrides, id)
  if (isUnboundBinding(binding)) return []
  const keys = binding.split(' ').filter(Boolean)
  return keys.length > 0 ? [keys] : []
}

function notify(message: string): void {
  useToastStore.getState().addToast(message, 'info')
}

/**
 * Review mode for one pane's editor. The editor is reused across notes, so
 * the review always ends before the pane shows another note: the layout
 * effect below runs ahead of the pane's own document swap.
 */
export function useAgentEditReview(options: {
  viewRef: MutableRefObject<EditorView | null>
  reviewCompartmentRef: MutableRefObject<Compartment | null>
  livePreviewCompartmentRef: MutableRefObject<Compartment | null>
  /** Live Preview's extensions as the pane would have them now. */
  livePreviewExtensions: () => Extension
  activePath: string | null
}): {
  session: AgentReviewSession | null
  /** Review `path` as soon as this pane's editor shows it (now, or once it
   *  has opened the note and left Preview). */
  requestReview: (path: string) => void
  /** Called by the pane whenever its editor may have become ready. */
  flushPendingReview: () => void
  stopReview: () => void
  acceptAll: () => void
  rejectAll: () => void
} {
  const { viewRef, reviewCompartmentRef, livePreviewCompartmentRef, livePreviewExtensions, activePath } = options
  const [session, setSession] = useState<AgentReviewSession | null>(null)
  const sessionRef = useRef<AgentReviewSession | null>(null)
  sessionRef.current = session
  const pendingRef = useRef<string | null>(null)

  const stopReview = useCallback(() => {
    const view = viewRef.current
    const compartment = reviewCompartmentRef.current
    if (view && compartment && sessionRef.current) {
      const effects = [compartment.reconfigure([])]
      const livePreview = livePreviewCompartmentRef.current
      if (livePreview) effects.push(livePreview.reconfigure(livePreviewExtensions()))
      view.dispatch({ effects })
    }
    sessionRef.current = null
    setSession(null)
  }, [viewRef, reviewCompartmentRef, livePreviewCompartmentRef, livePreviewExtensions])

  const startReview = useCallback(
    async (path: string) => {
      const read = window.zen.readAgentEdit
      const write = window.zen.writeAgentEdit
      const view = viewRef.current
      const compartment = reviewCompartmentRef.current
      if (!read || !write || !view || !compartment || path !== activePath) return
      const record = await read(path)
      if (!record) {
        await useStore.getState().refreshAgentEdit(path)
        return
      }
      const current = view.state.doc.toString()
      const original = agentEditBaseline(record, current)
      if (original === current) {
        // The assistant put its change back, or every line it touched has
        // since been rewritten: nothing is left to decide.
        await write(path, null)
        await useStore.getState().refreshAgentEdit(path)
        notify('Nothing left to review in this note.')
        return
      }
      const review = await loadAgentReview()
      if (viewRef.current !== view || path !== activePath) return
      const agent = latestAgentEdit(record)?.agent ?? 'Assistant'
      let latest: AgentEditRecord = record

      const extension = review.agentReviewExtension({
        original,
        keys: {
          next: keySpecs('editor.reviewNextChange'),
          previous: keySpecs('editor.reviewPreviousChange'),
          accept: keySpecs('editor.reviewAcceptChange'),
          reject: keySpecs('editor.reviewRejectChange'),
          vim: useStore.getState().vimMode
            ? {
                next: vimSequence('vim.reviewNextChange'),
                previous: vimSequence('vim.reviewPreviousChange'),
                accept: vimSequence('vim.reviewAcceptChange'),
                reject: vimSequence('vim.reviewRejectChange')
              }
            : undefined
        },
        onDecision: (comparison, remaining) => {
          void (async () => {
            // A rejected change is an edit: save it before the record says it
            // was decided, so a crash cannot leave the two disagreeing.
            await useStore.getState().persistNote(path)
            latest = (await read(path)) ?? latest
            if (remaining === 0) {
              await write(path, null)
              stopReview()
              notify(`Every change from ${agent} is decided.`)
            } else {
              await write(path, reviewedAgentEdit(latest, comparison, view.state.doc.toString()))
            }
            await useStore.getState().refreshAgentEdit(path)
          })().catch((error) => console.error('Saving the review failed', error))
        },
        onRemaining: (remaining) => {
          setSession((s) => (s && s.remaining !== remaining ? { ...s, remaining } : s))
        },
        onExit: stopReview
      })

      const effects = [compartment.reconfigure(extension)]
      const livePreview = livePreviewCompartmentRef.current
      // A diff is about the Markdown itself, so the review shows the source.
      if (livePreview) effects.push(livePreview.reconfigure([]))
      view.dispatch({ effects, selection: { anchor: 0 } })
      review.nextChange(view)
      view.focus()
      const next = { path, agent, remaining: review.remainingChanges(view.state) }
      sessionRef.current = next
      setSession(next)
    },
    [activePath, viewRef, reviewCompartmentRef, livePreviewCompartmentRef, stopReview]
  )

  // The pane is about to show another note: leave the review first.
  useLayoutEffect(() => {
    if (sessionRef.current && sessionRef.current.path !== activePath) stopReview()
  }, [activePath, stopReview])

  const flushPendingReview = useCallback(() => {
    const path = pendingRef.current
    if (!path || path !== activePath || !viewRef.current) return
    if (useStore.getState().noteContents[path] == null) return
    pendingRef.current = null
    void startReview(path)
  }, [activePath, viewRef, startReview])

  const requestReview = useCallback(
    (path: string) => {
      pendingRef.current = path
      flushPendingReview()
    },
    [flushPendingReview]
  )

  const acceptAll = useCallback(() => {
    const view = viewRef.current
    if (!view) return
    void loadAgentReview().then((review) => review.acceptAllChanges(view))
  }, [viewRef])

  const rejectAll = useCallback(() => {
    const view = viewRef.current
    if (!view) return
    void loadAgentReview().then((review) => review.rejectAllChanges(view))
  }, [viewRef])

  return { session, requestReview, flushPendingReview, stopReview, acceptAll, rejectAll }
}
