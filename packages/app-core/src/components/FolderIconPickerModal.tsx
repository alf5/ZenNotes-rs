import { useRef } from 'react'
import type { FolderIconId } from '@shared/ipc'
import { useStore } from '../store'
import { handlePickerGridKey } from '../lib/picker-grid'
import { FOLDER_ICON_OPTIONS } from './FolderIcons'
import { Modal } from './ui/Modal'
import { Button } from './ui/Button'

export function FolderIconPickerModal({
  targetLabel,
  currentIconId,
  onSelect,
  onReset,
  onCancel
}: {
  targetLabel: string
  currentIconId: FolderIconId | null
  onSelect: (iconId: FolderIconId) => void
  onReset: () => void
  onCancel: () => void
}): JSX.Element {
  const vimMode = useStore((s) => s.vimMode)
  // Opens on the icon already set, so Enter right away keeps it.
  const currentRef = useRef<HTMLButtonElement>(null)
  return (
    <Modal size="md" layer="modal" onClose={onCancel} initialFocus={currentRef}>
      <Modal.Header
        title="Choose icon"
        description={
          <>
            Select a sidebar icon for{' '}
            <span className="font-medium text-ink-700">{targetLabel}</span>.
          </>
        }
      />
      <Modal.Body>
        <div
          className="grid grid-cols-2 gap-2 sm:grid-cols-3"
          onKeyDown={(e) => handlePickerGridKey(e, vimMode)}
        >
          {FOLDER_ICON_OPTIONS.map((option) => {
            const active = option.id === currentIconId
            return (
              <button
                key={option.id}
                ref={active ? currentRef : undefined}
                type="button"
                data-picker-option
                onClick={() => onSelect(option.id)}
                className={[
                  'flex items-center gap-3 rounded-xl border px-3 py-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50',
                  active
                    ? 'border-accent bg-accent/10 text-accent'
                    : 'border-paper-300 bg-paper-50 text-ink-800 hover:border-paper-400 hover:bg-paper-200/70'
                ].join(' ')}
              >
                <span className={active ? 'text-accent' : 'text-ink-500'}>{option.icon}</span>
                <span className="truncate text-sm font-medium">{option.label}</span>
              </button>
            )
          })}
        </div>
      </Modal.Body>
      <Modal.Footer>
        <Button variant="secondary" onClick={onCancel}>
          Cancel
        </Button>
        {currentIconId && (
          <Button variant="ghost" onClick={onReset}>
            Reset icon
          </Button>
        )}
      </Modal.Footer>
    </Modal>
  )
}
