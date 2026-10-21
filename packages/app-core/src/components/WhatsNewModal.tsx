import { useRef, type KeyboardEvent as ReactKeyboardEvent } from 'react'
import { useStore } from '../store'
import { requestSettingsTarget } from '../lib/settings-navigation'
import { CLOUD_ANNOUNCEMENT, RELEASE_NOTES_URL, type WhatsNewEntry } from '../lib/whats-new'
import { Button, Modal } from './ui'

const TITLE_ID = 'whats-new-title'
const SCROLL_STEP = 56

function openExternal(url: string): void {
  window.open(url, '_blank', 'noopener,noreferrer')
}

function CloudCard({ onSetUp }: { onSetUp: () => void }): JSX.Element {
  return (
    <section data-whats-new-cloud="" className="rounded-lg border border-accent/30 bg-accent/10 px-4 py-3">
      <div className="text-sm font-semibold text-ink-900">{CLOUD_ANNOUNCEMENT.title}</div>
      <p className="mt-1 text-sm leading-6 text-ink-700">{CLOUD_ANNOUNCEMENT.body}</p>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button variant="primary" data-whats-new-cloud-setup="" onClick={onSetUp}>
          {CLOUD_ANNOUNCEMENT.settingsLabel}
        </Button>
        <Button variant="ghost" onClick={() => openExternal(CLOUD_ANNOUNCEMENT.link.url)}>
          {CLOUD_ANNOUNCEMENT.link.label}
        </Button>
      </div>
    </section>
  )
}

function Highlights({ entry, showVersion }: { entry: WhatsNewEntry; showVersion: boolean }): JSX.Element {
  return (
    <section data-whats-new-version={entry.version}>
      <div className="text-xs font-semibold uppercase tracking-wide text-ink-500">
        {showVersion ? `New in ${entry.version}` : 'In this update'}
      </div>
      <ul className="mt-2 space-y-3">
        {entry.highlights.map((item) => (
          <li key={item.title}>
            <div className="text-sm font-medium text-ink-900">{item.title}</div>
            <div className="text-sm leading-6 text-ink-500">{item.body}</div>
          </li>
        ))}
      </ul>
    </section>
  )
}

/**
 * What's New: opened by the first launch after every update, and from the
 * palette and `:whatsnew` (lib/whats-new.ts holds the entries and the rules).
 * Escape, Enter on Close, or the backdrop close it. The arrows scroll it, and
 * j / k in Vim mode.
 */
export default function WhatsNewModal(): JSX.Element | null {
  const view = useStore((s) => s.whatsNew)
  const close = useStore((s) => s.closeWhatsNew)
  const vimMode = useStore((s) => s.vimMode)
  const closeRef = useRef<HTMLButtonElement | null>(null)
  const bodyRef = useRef<HTMLDivElement | null>(null)
  if (!view) return null

  const setUpCloud = (): void => {
    close()
    requestSettingsTarget('cloud')
    useStore.getState().setSettingsOpen(true)
  }

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>): void => {
    if (e.metaKey || e.ctrlKey || e.altKey) return
    const down = e.key === 'ArrowDown' || (vimMode && e.key === 'j')
    const up = e.key === 'ArrowUp' || (vimMode && e.key === 'k')
    if (!down && !up) return
    e.preventDefault()
    bodyRef.current?.scrollBy({ top: down ? SCROLL_STEP : -SCROLL_STEP })
  }

  return (
    <Modal
      size="md"
      align="center"
      onClose={close}
      labelledBy={TITLE_ID}
      initialFocus={closeRef}
      data={{ 'data-whats-new': '' }}
    >
      <div onKeyDown={onKeyDown}>
        <Modal.Header titleId={TITLE_ID} title={`What’s new in ZenNotes ${view.version}`} />
        <div ref={bodyRef} data-whats-new-body="" className="max-h-[60vh] space-y-5 overflow-y-auto px-5 py-4">
          {view.showCloud && <CloudCard onSetUp={setUpCloud} />}
          {view.entries.length > 0 ? (
            view.entries.map((entry) => (
              <Highlights key={entry.version} entry={entry} showVersion={view.entries.length > 1} />
            ))
          ) : (
            <p className="text-sm leading-6 text-ink-500">
              What changed in {view.version} is in the full release notes.
            </p>
          )}
        </div>
        <Modal.Footer className="items-center justify-between">
          <Button variant="ghost" onClick={() => openExternal(RELEASE_NOTES_URL(view.version))}>
            Full release notes
          </Button>
          <Button ref={closeRef} variant="secondary" data-whats-new-close="" onClick={close}>
            Close
          </Button>
        </Modal.Footer>
      </div>
    </Modal>
  )
}
