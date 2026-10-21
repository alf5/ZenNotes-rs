// @vitest-environment jsdom

import { act, createElement, Fragment } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  CloudSyncPendingConflict,
  CloudSyncPendingConflictDetails,
  CloudSyncRunSummary,
} from "@zennotes/bridge-contract/cloud-sync";
import {
  clearCloudSyncStatus,
  cloudSyncAttentionItems,
  openPendingCloudReview,
  useCloudSyncStatusStore,
} from "../lib/cloud-auto-sync";
import { useStore } from "../store";
import { CloudConflictNoteBanner } from "./CloudConflictNoteBanner";
import { CloudConflictReviewHost } from "./CloudConflictReviewHost";

const bridge = vi.hoisted(() => ({
  getCloudConflict: vi.fn(),
  releaseCloudConflictReview: vi.fn(),
  saveCloudConflictDraft: vi.fn(),
  resolveCloudConflict: vi.fn(),
  syncCloudVault: vi.fn(),
}));

vi.mock("@zennotes/bridge-contract/bridge", () => ({
  getZenBridge: () => bridge,
}));

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

function pending(
  id: string,
  path: string,
  overrides: Partial<CloudSyncPendingConflict> = {},
): CloudSyncPendingConflict {
  return {
    id,
    item_id: id,
    path,
    cloud_path: path,
    kind: "content",
    can_merge: true,
    has_base: true,
    ...overrides,
  };
}

function summaryWith(...conflicts: CloudSyncPendingConflict[]): CloudSyncRunSummary {
  return {
    cursor: 4,
    pulled: 1,
    pushed: 0,
    conflicts: [],
    bootstrap_conflicts: [],
    local_conflicts: [],
    pending_conflicts: conflicts,
  };
}

function detailsFor(conflict: CloudSyncPendingConflict): CloudSyncPendingConflictDetails {
  const version = (text: string) => ({
    path: conflict.path,
    revision: 2,
    sha256: `hash-${text}`,
    byte_length: text.length,
    media_type: "text/markdown",
    text,
    deleted: false,
  });
  return {
    conflict,
    base: version("base"),
    local: version("local"),
    cloud: version("cloud"),
    suggested_text: "local",
    draft_text: null,
    changes: [],
    parts: [],
  };
}

describe("the conflicted-note banner", () => {
  let host: HTMLDivElement;
  let root: Root;
  let originalStore: ReturnType<typeof useStore.getState>;

  beforeEach(() => {
    clearCloudSyncStatus();
    originalStore = useStore.getState();
    useStore.setState({ vimMode: false, keymapOverrides: {} });
    bridge.getCloudConflict.mockReset();
    bridge.releaseCloudConflictReview.mockReset().mockResolvedValue(undefined);
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });

  afterEach(() => {
    act(() => root.unmount());
    host.remove();
    clearCloudSyncStatus();
    useStore.setState(originalStore, true);
  });

  const banners = (): HTMLElement[] => [
    ...host.querySelectorAll<HTMLElement>("[data-cloud-conflict-banner]"),
  ];

  it("shows over a note waiting in the queue, under either of its names, and nowhere else", () => {
    useCloudSyncStatusStore.setState({
      lastSummary: summaryWith(
        pending("moved", "Plans/Trip.md", { cloud_path: "Archive/Trip.md", kind: "move" }),
      ),
    });
    act(() =>
      root.render(
        createElement(
          Fragment,
          null,
          createElement(CloudConflictNoteBanner, { path: "Plans/Trip.md" }),
          // Sync compares paths case-folded, and the Cloud name counts too.
          createElement(CloudConflictNoteBanner, { path: "archive/trip.md" }),
          createElement(CloudConflictNoteBanner, { path: "Plans/Other.md" }),
          // A name sync cannot carry is never in the queue, and must not throw.
          createElement(CloudConflictNoteBanner, { path: "Plans/Trip: notes.md" }),
        ),
      ),
    );
    expect(banners()).toHaveLength(2);

    act(() => useCloudSyncStatusStore.setState({ lastSummary: summaryWith() }));
    expect(banners()).toHaveLength(0);
  });

  it("says sync is paused for an edited note, and keeps the queue's own words for the other kinds", () => {
    const content = pending("content", "Today.md");
    const deleted = pending("deleted", "Gone.md", { kind: "delete" });
    const moved = pending("moved", "Moved.md", { kind: "move" });
    const named = pending("named", "Named.md", { kind: "path" });
    const summary = summaryWith(content, deleted, moved, named);
    useCloudSyncStatusStore.setState({ lastSummary: summary });
    act(() =>
      root.render(
        createElement(
          Fragment,
          null,
          ...[content, deleted, moved, named].map((conflict) =>
            createElement(CloudConflictNoteBanner, { key: conflict.id, path: conflict.path }),
          ),
        ),
      ),
    );

    const messages = banners().map(
      (banner) => banner.querySelector('[role="status"]')?.textContent,
    );
    expect(messages[0]).toBe(
      "Sync is paused for this note. It changed on this device and on another device.",
    );
    const settingsWording = cloudSyncAttentionItems(summary).map((item) => item.detail);
    expect(messages.slice(1)).toEqual(settingsWording.slice(1));
  });

  it("opens the review on this note's conflict, not the first one in the queue", async () => {
    const first = pending("first", "Alpha.md");
    const own = pending("own", "Beta.md");
    bridge.getCloudConflict.mockImplementation(async (id: string) =>
      detailsFor(id === own.id ? own : first),
    );
    useCloudSyncStatusStore.setState({
      vaultName: "Notes",
      lastSummary: summaryWith(first, own),
    });
    act(() =>
      root.render(
        createElement(
          Fragment,
          null,
          createElement(CloudConflictNoteBanner, { path: own.path }),
          createElement(CloudConflictReviewHost),
        ),
      ),
    );

    const review = host.querySelector<HTMLButtonElement>("[data-cloud-conflict-review]")!;
    expect(review.textContent).toBe("Review");
    // The press leaves focus where it was (the editor), so the queue hands it
    // back there when it closes.
    const press = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    review.dispatchEvent(press);
    expect(press.defaultPrevented).toBe(true);

    await act(async () => {
      review.click();
      await Promise.resolve();
    });

    expect(useCloudSyncStatusStore.getState()).toMatchObject({
      conflictReviewOpen: true,
      conflictReviewStartId: own.id,
    });
    const dialog = document.body.querySelector<HTMLElement>("[data-cloud-conflict-dialog]");
    const selected = dialog?.querySelector<HTMLButtonElement>('button[aria-pressed="true"]');
    expect(selected?.textContent).toBe(own.path);
    expect(bridge.getCloudConflict).toHaveBeenCalledWith(own.id, expect.any(String));
    expect(bridge.getCloudConflict).not.toHaveBeenCalledWith(first.id, expect.any(String));
  });

  it("names the leader binding only while Vim mode is on", () => {
    useCloudSyncStatusStore.setState({ lastSummary: summaryWith(pending("own", "Beta.md")) });
    const hint = (): string | null =>
      host.querySelector("[data-cloud-conflict-banner] kbd")?.textContent ?? null;

    act(() => root.render(createElement(CloudConflictNoteBanner, { path: "Beta.md" })));
    expect(hint()).toBeNull();
    expect(host.textContent).not.toContain("Space");

    act(() => useStore.setState({ vimMode: true }));
    expect(hint()).toBe("Space r");

    act(() => useStore.setState({ keymapOverrides: { "vim.leaderCloudConflicts": "x" } }));
    expect(hint()).toBe("Space x");

    // Half a chord cannot be pressed, so it is not shown.
    act(() => useStore.setState({ keymapOverrides: { "vim.leaderCloudConflicts": "" } }));
    expect(hint()).toBeNull();

    act(() => useStore.setState({ vimMode: false, keymapOverrides: {} }));
    expect(hint()).toBeNull();
  });

  it("lets the leader binding and the palette land where the banner's Review does", () => {
    const first = pending("first", "Alpha.md");
    const own = pending("own", "Beta.md");
    useCloudSyncStatusStore.setState({ lastSummary: summaryWith(first, own) });

    openPendingCloudReview(own.path);
    expect(useCloudSyncStatusStore.getState().conflictReviewStartId).toBe(own.id);

    // A note with nothing waiting opens the queue at its start.
    openPendingCloudReview("Elsewhere.md");
    expect(useCloudSyncStatusStore.getState().conflictReviewStartId).toBeNull();
    openPendingCloudReview(null);
    expect(useCloudSyncStatusStore.getState()).toMatchObject({
      conflictReviewOpen: true,
      conflictReviewStartId: null,
    });
  });
});
