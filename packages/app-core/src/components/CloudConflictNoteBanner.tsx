import {
  cloudConflictNoteMessage,
  openCloudConflictReview,
  pendingCloudConflictForPath,
  useCloudSyncStatusStore,
} from "../lib/cloud-auto-sync";
import { getLeaderChordDisplay } from "../lib/keymaps";
import { useStore } from "../store";
import { Button } from "./ui/Button";

/**
 * Sits over a note while it waits in the Cloud conflict queue. Sync is paused
 * for that one note: this device keeps its own text and the other device's
 * waits in the queue. The status bar that reports the queue is hidden on a
 * phone and in zen mode, so the note itself has to say so.
 */
export function CloudConflictNoteBanner({
  path,
}: {
  path: string;
}): JSX.Element | null {
  const conflict = useCloudSyncStatusStore((state) =>
    pendingCloudConflictForPath(state.lastSummary, path),
  );
  // With Vim mode off nothing on screen names a Vim key.
  const keyHint = useStore((state) =>
    state.vimMode
      ? getLeaderChordDisplay(state.keymapOverrides, "vim.leaderCloudConflicts")
      : "",
  );
  if (conflict === null) return null;
  return (
    <div
      data-cloud-conflict-banner=""
      className="flex shrink-0 items-center gap-3 border-b border-warning/30 bg-warning/10 px-4 py-1.5 text-sm"
    >
      <svg
        aria-hidden="true"
        viewBox="0 0 20 20"
        fill="none"
        className="h-4 w-4 shrink-0 text-warning"
      >
        <circle cx="10" cy="10" r="7" stroke="currentColor" strokeWidth="1.6" />
        <path
          d="M10 6.5v4.2M10 13.8v.1"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
        />
      </svg>
      <p role="status" className="min-w-0 flex-1 text-ink-800">
        {cloudConflictNoteMessage(conflict)}
      </p>
      {keyHint && (
        <kbd
          data-keyboard-hints=""
          className="shrink-0 rounded bg-paper-200/80 px-1.5 py-0.5 text-xs text-ink-500"
        >
          {keyHint}
        </kbd>
      )}
      <Button
        data-cloud-conflict-review=""
        variant="secondary"
        className="shrink-0"
        // A press must not pull the keyboard out of the editor: the queue
        // returns focus to whatever held it, and that should be the note.
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => openCloudConflictReview(conflict.id)}
      >
        Review
      </Button>
    </div>
  );
}
