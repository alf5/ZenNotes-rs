import {
  pendingCloudConflictForPath,
  useCloudSyncStatusStore,
} from "../lib/cloud-auto-sync";

export function CloudTaskConflictIndicator({
  path,
}: {
  path: string;
}): JSX.Element | null {
  const pending = useCloudSyncStatusStore(
    (state) => pendingCloudConflictForPath(state.lastSummary, path) !== null,
  );
  if (!pending) return null;
  return (
    <span
      className="shrink-0 rounded bg-amber-500/10 px-1.5 py-0.5 text-xs text-ink-700"
      title="Tasks from your local note stay visible while its Cloud conflict is pending. Review sync changes to resolve it."
    >
      Conflict pending
    </span>
  );
}
