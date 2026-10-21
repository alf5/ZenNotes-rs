import { useEffect, useMemo, useState } from 'react'
import { createPortal } from 'react-dom'
import {
  followEditorLinkHint,
  visibleEditorLinkHints,
  type HintRect
} from '../lib/editor-link-hints'
import { generateHintLabels } from '../lib/vim-nav'

interface HintCandidate {
  rect: HintRect
  /** Labels are handed out in document order of this node. */
  anchor: Node
  /** Follows the target; returns the element it clicked, when it was one. */
  activate: () => HTMLElement | undefined
}

interface HintTarget {
  label: string
  left: number
  top: number
  activate: () => HTMLElement | undefined
}

function getVisibleInteractiveElements(): HTMLElement[] {
  const selectors = [
    'button:not([disabled])',
    'a[href]',
    // A <details> toggle, such as a foldable callout's title in Preview (#853).
    'summary',
    '[role="button"]',
    '[role="link"]',
    'input:not([disabled])',
    'select:not([disabled])',
    'textarea:not([disabled])'
  ].join(', ')

  const all = document.querySelectorAll<HTMLElement>(selectors)
  const visible: HTMLElement[] = []

  for (const el of all) {
    // Skip elements inside the hint overlay itself
    if (el.closest('.vim-hint-overlay')) continue
    if (el.matches('[data-vim-hint-ignore]') || el.closest('[data-vim-hint-ignore]')) continue
    // Skip hidden elements
    const rect = el.getBoundingClientRect()
    if (rect.width === 0 || rect.height === 0) continue
    // Skip off-screen elements
    if (
      rect.bottom < 0 ||
      rect.top > window.innerHeight ||
      rect.right < 0 ||
      rect.left > window.innerWidth
    ) continue
    // Skip elements hidden by CSS
    const style = window.getComputedStyle(el)
    if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') continue
    visible.push(el)
  }
  return visible
}

function documentOrder(a: Node, b: Node): number {
  if (a === b) return 0
  return a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1
}

function getHintCandidates(): HintCandidate[] {
  const candidates: HintCandidate[] = getVisibleInteractiveElements().map((element) => ({
    rect: element.getBoundingClientRect(),
    anchor: element,
    activate: () => {
      element.click()
      element.focus()
      return element
    }
  }))
  // The Edit view draws its links as text, not elements (#894). Each joins the
  // page's targets where it is drawn, so labels run through a note top to
  // bottom as they do over Preview's links; the sort is stable, so links that
  // share a text node keep their order.
  for (const link of visibleEditorLinkHints()) {
    candidates.push({
      rect: link.rect,
      anchor: link.anchor,
      activate: () => {
        followEditorLinkHint(link)
        return undefined
      }
    })
  }
  return candidates.sort((a, b) => documentOrder(a.anchor, b.anchor))
}

function overlaps(
  a: { left: number; top: number; width: number; height: number },
  b: { left: number; top: number; width: number; height: number }
): boolean {
  return !(
    a.left + a.width <= b.left ||
    b.left + b.width <= a.left ||
    a.top + a.height <= b.top ||
    b.top + b.height <= a.top
  )
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max)
}

function getHintPlacement(
  rect: HintRect,
  label: string,
  placed: Array<{ left: number; top: number; width: number; height: number }>
): { left: number; top: number } {
  const width = Math.max(18, label.length * 8 + 10)
  const height = 18
  const gutter = 4
  const maxLeft = Math.max(gutter, window.innerWidth - width - gutter)
  const maxTop = Math.max(gutter, window.innerHeight - height - gutter)
  const candidates = [
    { left: rect.left + gutter, top: rect.top + gutter },
    { left: rect.left + gutter, top: rect.top - height - gutter },
    { left: rect.right - width - gutter, top: rect.top + gutter },
    { left: rect.right - width - gutter, top: rect.top - height - gutter },
    { left: rect.left + gutter, top: rect.bottom - height - gutter },
    { left: rect.right - width - gutter, top: rect.bottom - height - gutter }
  ].map((pos) => ({
    left: clamp(pos.left, gutter, maxLeft),
    top: clamp(pos.top, gutter, maxTop)
  }))

  for (const candidate of candidates) {
    const box = { ...candidate, width, height }
    if (!placed.some((existing) => overlaps(box, existing))) {
      return candidate
    }
  }

  const fallback = candidates[0]
  for (let offset = 0; offset < window.innerHeight; offset += height + gutter) {
    const candidate = {
      left: fallback.left,
      top: clamp(fallback.top + offset, gutter, maxTop)
    }
    const box = { ...candidate, width, height }
    if (!placed.some((existing) => overlaps(box, existing))) {
      return candidate
    }
  }

  return fallback
}

export function HintOverlay({
  onActivate,
  onCancel
}: {
  onActivate: (element?: HTMLElement) => void
  onCancel: () => void
}): JSX.Element | null {
  const [buffer, setBuffer] = useState('')

  const targets = useMemo<HintTarget[]>(() => {
    const candidates = getHintCandidates()
    const labels = generateHintLabels(candidates.length)
    const placed: Array<{ left: number; top: number; width: number; height: number }> = []

    return candidates.map((candidate, i) => {
      const label = labels[i]
      const position = getHintPlacement(candidate.rect, label, placed)
      placed.push({
        ...position,
        width: Math.max(18, label.length * 8 + 10),
        height: 18
      })
      return {
        label,
        activate: candidate.activate,
        ...position
      }
    })
  }, [])

  // Filter targets based on current buffer
  const matching = useMemo(
    () => targets.filter((t) => t.label.startsWith(buffer)),
    [targets, buffer]
  )

  useEffect(() => {
    // If exactly one match, click it
    if (buffer.length > 0 && matching.length === 1) {
      const target = matching[0]
      // Small delay so the user sees the match before it fires
      const t = setTimeout(() => {
        onActivate(target.activate())
      }, 50)
      return () => clearTimeout(t)
    }
    // If no matches, exit
    if (buffer.length > 0 && matching.length === 0) {
      onCancel()
    }
    return undefined
  }, [buffer, matching, onActivate, onCancel])

  useEffect(() => {
    const handler = (e: KeyboardEvent): void => {
      e.preventDefault()
      e.stopPropagation()

      if (e.key === 'Escape') {
        onCancel()
        return
      }

      // A bare modifier is not a choice. Tap-hold home-row mods (f/j as
      // Shift, d/k as Control via Kanata and friends) emit the modifier when
      // a label letter is held a beat too long; canceling on it made every
      // label containing one of those letters a dead end (#526). Swallow the
      // modifier and keep waiting for the letter.
      if (
        e.key === 'Shift' ||
        e.key === 'Control' ||
        e.key === 'Alt' ||
        e.key === 'Meta' ||
        e.key === 'CapsLock' ||
        e.key === 'AltGraph'
      ) {
        return
      }

      // Letters match case-insensitively: a Shift that misfired (or Caps
      // Lock left on) should still choose the label, not exit hint mode.
      if (e.key.length === 1 && /^[a-zA-Z]$/.test(e.key)) {
        setBuffer((b) => b + e.key.toLowerCase())
        return
      }

      // Backspace removes last character
      if (e.key === 'Backspace') {
        setBuffer((b) => (b.length > 0 ? b.slice(0, -1) : b))
        return
      }

      // Any other key exits
      onCancel()
    }

    window.addEventListener('keydown', handler, true)
    return () => window.removeEventListener('keydown', handler, true)
  }, [onCancel])

  useEffect(() => {
    if (targets.length === 0) onCancel()
  }, [targets.length, onCancel])

  if (targets.length === 0) return null

  return createPortal(
    <div data-vim-hint-overlay className="vim-hint-overlay" style={{ position: 'fixed', inset: 0, zIndex: 9999, pointerEvents: 'none' }}>
      {targets.map((t) => {
        const isMatch = t.label.startsWith(buffer)
        const matchedPart = buffer
        const remainingPart = t.label.slice(buffer.length)
        return (
          <span
            key={t.label}
            className={isMatch ? 'vim-hint' : 'vim-hint vim-hint-dim'}
            style={{
              position: 'absolute',
              left: t.left,
              top: t.top
            }}
          >
            {matchedPart && <span className="vim-hint-matched">{matchedPart}</span>}
            {remainingPart}
          </span>
        )
      })}
    </div>,
    document.body
  )
}
