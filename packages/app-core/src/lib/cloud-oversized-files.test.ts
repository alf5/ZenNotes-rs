import { describe, expect, it } from "vitest";
import type {
  CloudSyncConflict,
  CloudSyncRunSummary,
} from "@zennotes/bridge-contract/cloud-sync";
import {
  oversizedCloudFileNotice,
  oversizedCloudFiles,
} from "./cloud-oversized-files";

function tooLarge(
  item: string,
  path: string | null,
  limit: number | null = 10_000_000,
): CloudSyncConflict {
  return {
    operation_id: `op-${item}`,
    item_id: item,
    code: "FILE_SIZE_LIMIT_EXCEEDED",
    current_revision: null,
    current_path: null,
    path,
    ...(limit === null
      ? {}
      : {
          capacity: {
            dimension: "sync_max_file_bytes",
            used: 0,
            reserved: 0,
            limit,
            projected: limit * 3,
            can_retry_after_reduction: true,
          },
        }),
  };
}

function run(...conflicts: CloudSyncConflict[]): CloudSyncRunSummary {
  return {
    cursor: 1,
    pulled: 0,
    pushed: 0,
    bootstrap_conflicts: [],
    local_conflicts: [],
    conflicts,
  };
}

const revisionConflict: CloudSyncConflict = {
  operation_id: "op-rev",
  item_id: "rev",
  code: "REVISION_CONFLICT",
  current_revision: 3,
  current_path: null,
  path: "notes/Plan.md",
};

describe("oversizedCloudFiles", () => {
  it("is one shared empty set whenever no file is over the per-file limit", () => {
    const none = oversizedCloudFiles(null);
    expect(none.size).toBe(0);
    expect(oversizedCloudFiles(run())).toBe(none);
    expect(oversizedCloudFiles(run(revisionConflict))).toBe(none);
    // Storage running out is a Cloud limit too, but no one file is to blame.
    const storageFull = run({
      operation_id: "op-q",
      item_id: "q",
      code: "QUOTA_EXCEEDED",
      current_revision: null,
      current_path: null,
      path: "assets/clip.mp4",
      capacity: {
        dimension: "sync_active_bytes",
        used: 10,
        reserved: 0,
        limit: 10,
        projected: 11,
        can_retry_after_reduction: true,
      },
    });
    expect(oversizedCloudFiles(storageFull)).toBe(none);
  });

  it("hands back the same set for a later run that leaves the same files at the same limits", () => {
    const first = oversizedCloudFiles(run(tooLarge("a", "assets/a.mov"), tooLarge("b", "assets/b.mov")));
    const reordered = oversizedCloudFiles(
      run(revisionConflict, tooLarge("b", "assets/b.mov"), tooLarge("a", "assets/a.mov")),
    );
    expect(reordered).toBe(first);
    expect([...first.keys()]).toEqual(["assets/a.mov", "assets/b.mov"]);

    const raised = oversizedCloudFiles(run(tooLarge("a", "assets/a.mov", 50_000_000), tooLarge("b", "assets/b.mov")));
    expect(raised).not.toBe(first);
    expect(raised.get("assets/a.mov")).toBe(50_000_000);
  });

  it("skips a conflict without a path, and a path sync cannot carry, without throwing", () => {
    const files = oversizedCloudFiles(run(tooLarge("x", null), tooLarge("y", "bad:name.mov")));
    expect(files.size).toBe(0);
  });
});

describe("oversizedCloudFileNotice", () => {
  it("names the limit the run reported", () => {
    const files = oversizedCloudFiles(run(tooLarge("v", "assets/IMG_2709.mov")));
    expect(oversizedCloudFileNotice(files, "assets/IMG_2709.mov")).toBe(
      "Not synced to Cloud: larger than the 10 MB file-size limit, so it stays on this device.",
    );
    expect(oversizedCloudFileNotice(files, "assets/other.mov")).toBeNull();
    expect(oversizedCloudFileNotice(files, null)).toBeNull();
  });

  it("still says it when the run named no limit", () => {
    const files = oversizedCloudFiles(run(tooLarge("v", "assets/clip.mp4", null)));
    expect(oversizedCloudFileNotice(files, "assets/clip.mp4")).toBe(
      "Not synced to Cloud: larger than the Cloud file-size limit, so it stays on this device.",
    );
  });

  it("matches paths the way sync compares them: case-folded and Unicode-normalized", () => {
    const decomposed = "assets/Café Tour.MOV";
    const files = oversizedCloudFiles(run(tooLarge("v", decomposed)));
    expect(oversizedCloudFileNotice(files, "assets/café tour.mov")).not.toBeNull();
  });

  it("answers null instead of throwing for a path sync cannot carry", () => {
    const files = oversizedCloudFiles(run(tooLarge("v", "assets/clip.mp4")));
    expect(() => oversizedCloudFileNotice(files, "assets/bad:name.mp4")).not.toThrow();
    expect(oversizedCloudFileNotice(files, "assets/bad:name.mp4")).toBeNull();
  });
});
