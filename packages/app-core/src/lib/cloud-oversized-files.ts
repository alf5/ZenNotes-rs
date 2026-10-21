import type { CloudSyncRunSummary } from "@zennotes/bridge-contract/cloud-sync";
import {
  cloudSyncPathKeyOrNull,
  formatCloudBytes,
  oversizedFileConflicts,
} from "./cloud-auto-sync";

/**
 * The files the last Cloud run left on this device for being over the
 * per-file limit, keyed the way sync compares paths (case-folded, Unicode
 * normalized), each with the limit it was over when the run reported one.
 */
export type OversizedCloudFiles = ReadonlyMap<string, number | null>;

const NO_OVERSIZED_CLOUD_FILES: OversizedCloudFiles = new Map();

// Every embed in every open note asks about the same summary, and the editor
// and the reading view redraw only when the answer is a different object. So
// one summary gives one answer, and a later run that left the same files at
// the same limits gives the same object back: a sync that changed nothing
// here redraws nothing.
let lastSummary: CloudSyncRunSummary | null | undefined;
let lastSignature = "";
let lastFiles: OversizedCloudFiles = NO_OVERSIZED_CLOUD_FILES;

export function oversizedCloudFiles(
  summary: CloudSyncRunSummary | null,
): OversizedCloudFiles {
  if (summary === lastSummary) return lastFiles;
  lastSummary = summary;
  const entries: Array<[string, number | null]> = [];
  if (summary && summary.conflicts.length > 0) {
    for (const conflict of oversizedFileConflicts(summary)) {
      const key = conflict.path ? cloudSyncPathKeyOrNull(conflict.path) : null;
      if (key !== null) entries.push([key, conflict.capacity?.limit ?? null]);
    }
  }
  if (entries.length === 0) {
    lastSignature = "";
    lastFiles = NO_OVERSIZED_CLOUD_FILES;
    return lastFiles;
  }
  entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const signature = entries
    .map(([key, limit]) => `${key}\u0000${limit ?? ""}`)
    .join("\u0001");
  if (signature !== lastSignature) {
    lastSignature = signature;
    lastFiles = new Map(entries);
  }
  return lastFiles;
}

/**
 * The line an embed of `vaultRelPath` carries while sync keeps that file on
 * this device for its size; null for every other file.
 */
export function oversizedCloudFileNotice(
  files: OversizedCloudFiles,
  vaultRelPath: string | null,
): string | null {
  if (files.size === 0 || !vaultRelPath) return null;
  const key = cloudSyncPathKeyOrNull(vaultRelPath);
  if (key === null || !files.has(key)) return null;
  const limit = files.get(key);
  const limitName = limit == null ? "Cloud" : formatCloudBytes(limit);
  return `Not synced to Cloud: larger than the ${limitName} file-size limit, so it stays on this device.`;
}
