/**
 * Review mode: an assistant's edits to a note, shown inline in the note
 * itself (docs/ideas/agent-edit-review.md). CodeMirror's merge view draws
 * each change against the comparison text, the note without the assistant's
 * changes (@shared/agent-edits `agentEditBaseline`): the removed lines above
 * the new ones, with Accept and Reject on each.
 *
 * Accept moves the change into the comparison text, so it stops showing.
 * Reject puts the old text back into the note, an ordinary edit the editor
 * saves like any other. The note is read-only while it is being reviewed:
 * the only edits are the decisions, so the review always shows exactly the
 * assistant's changes and the user's progress can be saved as one text.
 *
 * This module is imported on demand (agent-review-runtime.ts), so
 * @codemirror/merge never rides the boot path.
 */
import {
  acceptChunk,
  getChunks,
  getOriginalDoc,
  goToNextChunk,
  goToPreviousChunk,
  rejectChunk,
  unifiedMergeView,
  updateOriginalDoc
} from '@codemirror/merge'
import { ChangeSet, EditorState, Prec, StateField, type Extension } from '@codemirror/state'
import { EditorView, keymap, type KeyBinding } from '@codemirror/view'
import { getCM } from '@replit/codemirror-vim'

/** Marks an editor that is reviewing; the Vim keys and the palette look for it. */
const reviewing = StateField.define<boolean>({
  create: () => true,
  update: (value) => value
})

export interface AgentReviewKeys {
  /** CodeMirror key specs (the modifier shortcuts, in or out of Vim mode). */
  next: string[]
  previous: string[]
  accept: string[]
  reject: string[]
  /** Vim normal-mode sequences, one key per entry: `[['d', 'p']]`. */
  vim?: {
    next: string[][]
    previous: string[][]
    accept: string[][]
    reject: string[][]
  }
}

export interface AgentReviewOptions {
  /** The comparison text: the note without the assistant's changes. */
  original: string
  /** CodeMirror key specs for the configurable review shortcuts. */
  keys: AgentReviewKeys
  /** After each decision: the comparison text so far, and how many changes are left. */
  onDecision: (original: string, remaining: number) => void
  /** Whenever the count may have changed, decisions or not: the assistant can
   *  edit the note again while it is being reviewed, and its new changes show
   *  up as changes too. */
  onRemaining?: (remaining: number) => void
  /** Escape, with nothing selected: leave the review, keeping what is undecided. */
  onExit: () => void
}

export function isReviewing(state: EditorState): boolean {
  return state.field(reviewing, false) ?? false
}

export function remainingChanges(state: EditorState): number {
  return getChunks(state)?.chunks.length ?? 0
}

export function nextChange(view: EditorView): boolean {
  return goToNextChunk(view)
}

export function previousChange(view: EditorView): boolean {
  return goToPreviousChunk(view)
}

export function acceptChange(view: EditorView): boolean {
  return acceptChunk(view)
}

export function rejectChange(view: EditorView): boolean {
  return rejectChunk(view)
}

/** Keep every remaining change: the comparison text becomes the note. */
export function acceptAllChanges(view: EditorView): boolean {
  const original = getOriginalDoc(view.state)
  if (original.eq(view.state.doc)) return false
  const changes = ChangeSet.of(
    [{ from: 0, to: original.length, insert: view.state.doc }],
    original.length
  )
  view.dispatch({
    effects: updateOriginalDoc.of({ doc: view.state.doc, changes }),
    userEvent: 'accept'
  })
  return true
}

/** Put back everything still undecided: the note becomes the comparison text. */
export function rejectAllChanges(view: EditorView): boolean {
  const original = getOriginalDoc(view.state)
  if (original.eq(view.state.doc)) return false
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: original },
    userEvent: 'revert'
  })
  return true
}

function control(type: 'accept' | 'reject', action: (event: MouseEvent) => void): HTMLElement {
  const button = document.createElement('button')
  button.type = 'button'
  button.className = `zen-review-control zen-review-${type}`
  button.textContent = type === 'accept' ? 'Accept' : 'Reject'
  button.title = type === 'accept' ? 'Keep this change' : 'Put back what this change replaced'
  // On mousedown, like the merge view's own buttons: a click would first move
  // the cursor, and the chunk is found from where the button sits.
  button.onmousedown = action
  return button
}

type VimStateLike = {
  insertMode?: boolean
  visualMode?: boolean
  inputState?: { operator?: unknown; keyBuffer?: unknown[] }
}

function vimState(view: EditorView): VimStateLike | null {
  return (getCM(view)?.state?.vim as VimStateLike | undefined) ?? null
}

function vimIdle(vim: VimStateLike): boolean {
  if (vim.insertMode || vim.visualMode) return false
  const input = vim.inputState
  return !input?.operator && !(input?.keyBuffer && input.keyBuffer.length > 0)
}

/**
 * Vim's diff-mode keys (`]c` `[c` `dp` `do`) for the review. They cannot be
 * Vim mappings: codemirror-vim runs a full match the moment it has one, so
 * `d` would always start the delete operator before `dp` could match. While
 * reviewing, this reads normal-mode keys ahead of Vim instead: a key that
 * continues one of the sequences is held, a complete sequence runs, and any
 * other key goes on to Vim. The note is read-only, so the `d` held while
 * waiting for `p` or `o` costs nothing.
 */
function vimReviewKeys(keys: NonNullable<AgentReviewKeys['vim']>): Extension {
  const sequences = [
    ...keys.next.map((seq) => ({ seq, run: nextChange })),
    ...keys.previous.map((seq) => ({ seq, run: previousChange })),
    ...keys.accept.map((seq) => ({ seq, run: acceptChange })),
    ...keys.reject.map((seq) => ({ seq, run: rejectChange }))
  ].filter((entry) => entry.seq.length > 0)
  let held: string[] = []
  return Prec.highest(
    EditorView.domEventHandlers({
      keydown(event, view) {
        if (event.key === 'Escape' && held.length > 0) {
          // A half-typed `d`: Escape cancels it, it does not end the review.
          held = []
          return true
        }
        const vim = vimState(view)
        if (
          !vim ||
          !vimIdle(vim) ||
          event.ctrlKey ||
          event.metaKey ||
          event.altKey ||
          event.key.length !== 1
        ) {
          held = []
          return false
        }
        const typed = [...held, event.key]
        const complete = sequences.find(
          ({ seq }) => seq.length === typed.length && seq.every((key, i) => key === typed[i])
        )
        if (complete) {
          held = []
          complete.run(view)
          return true
        }
        if (sequences.some(({ seq }) => seq.length > typed.length && typed.every((key, i) => seq[i] === key))) {
          held = typed
          return true
        }
        held = []
        return false
      }
    })
  )
}

/**
 * Escape leaves the review only when it has nothing else to do: a selection
 * (Vim's visual mode, a mouse drag) is cleared first, and in Vim mode Escape
 * still cancels a half-typed command (the `d` of `dp`) or leaves insert mode,
 * which the read-only note can still enter.
 */
function escapeLeavesReview(view: EditorView): boolean {
  if (!view.state.selection.main.empty) return false
  const vim = vimState(view)
  return !vim || vimIdle(vim)
}

export function agentReviewExtension(options: AgentReviewOptions): Extension {
  const bind = (specs: string[], run: (view: EditorView) => boolean): KeyBinding[] =>
    specs.map((key) => ({ key, run, preventDefault: true }))
  return [
    reviewing,
    // Scopes the review's colors and buttons in index.css above the merge
    // view's own theme, which is injected at run time.
    EditorView.editorAttributes.of({ class: 'zen-agent-review' }),
    unifiedMergeView({
      original: options.original,
      mergeControls: control,
      gutter: true,
      syntaxHighlightDeletions: true,
      collapseUnchanged: { margin: 3, minSize: 6 }
    }),
    EditorState.readOnly.of(true),
    options.keys.vim ? vimReviewKeys(options.keys.vim) : [],
    Prec.highest(
      keymap.of([
        ...bind(options.keys.next, nextChange),
        ...bind(options.keys.previous, previousChange),
        ...bind(options.keys.accept, acceptChange),
        ...bind(options.keys.reject, rejectChange),
        {
          key: 'Escape',
          run: (view) => {
            if (!escapeLeavesReview(view)) return false
            options.onExit()
            return true
          }
        }
      ])
    ),
    EditorView.updateListener.of((update) => {
      const decided = update.transactions.some(
        (tr) => tr.isUserEvent('accept') || tr.isUserEvent('revert')
      )
      if (decided) {
        options.onDecision(getOriginalDoc(update.state).toString(), remainingChanges(update.state))
      }
      if (decided || update.docChanged) options.onRemaining?.(remainingChanges(update.state))
    })
  ]
}
