/**
 * The always-loaded side of review mode (cm-agent-review.ts is imported on
 * demand, so @codemirror/merge stays off the boot path).
 */
type AgentReviewModule = typeof import('./cm-agent-review')

/** The window event that asks the active pane to review a note. */
export const REVIEW_AGENT_EDITS_EVENT = 'zen:review-agent-edits'

export function requestAgentEditReview(path: string): void {
  window.dispatchEvent(new CustomEvent(REVIEW_AGENT_EDITS_EVENT, { detail: { path } }))
}

let loaded: AgentReviewModule | null = null

export async function loadAgentReview(): Promise<AgentReviewModule> {
  loaded ??= await import('./cm-agent-review')
  return loaded
}
