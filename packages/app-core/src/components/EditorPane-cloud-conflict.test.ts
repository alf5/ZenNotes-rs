// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { NoteContent } from "@shared/ipc";
import type { CloudSyncRunSummary } from "@zennotes/bridge-contract/cloud-sync";
import { clearCloudSyncStatus, useCloudSyncStatusStore } from "../lib/cloud-auto-sync";
import type { PaneLeaf } from "../lib/pane-layout";
import type { PaneMode } from "../lib/pane-mode";
import { useStore } from "../store";
import { EditorPane } from "./EditorPane";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const note: NoteContent = {
  path: "inbox/Today.md",
  title: "Today",
  folder: "inbox",
  siblingOrder: 0,
  createdAt: 0,
  updatedAt: 0,
  size: 11,
  tags: [],
  wikilinks: [],
  assetEmbeds: [],
  hasAttachments: false,
  excerpt: "Hello there",
  body: "Hello there",
};

const pane: PaneLeaf = {
  kind: "leaf",
  id: "pane-1",
  tabs: [note.path],
  pinnedTabs: [],
  activeTab: note.path,
};

const waiting: CloudSyncRunSummary = {
  cursor: 3,
  pulled: 1,
  pushed: 0,
  conflicts: [],
  bootstrap_conflicts: [],
  local_conflicts: [],
  pending_conflicts: [
    {
      id: "today",
      item_id: "today",
      path: note.path,
      cloud_path: note.path,
      kind: "content",
      can_merge: true,
      has_base: true,
    },
  ],
};

describe("EditorPane over a note waiting in the Cloud conflict queue", () => {
  let host: HTMLDivElement;
  let root: Root;
  let originalStore: ReturnType<typeof useStore.getState>;

  beforeEach(() => {
    // jsdom has no layout; CodeMirror still measures.
    Object.defineProperty(Range.prototype, "getClientRects", {
      configurable: true,
      value: () => [],
    });
    Object.defineProperty(Range.prototype, "getBoundingClientRect", {
      configurable: true,
      value: () => new DOMRect(),
    });
    for (const method of ["scrollIntoView", "scrollTo"] as const) {
      Object.defineProperty(HTMLElement.prototype, method, {
        configurable: true,
        value: () => {},
      });
    }
    Object.defineProperty(window, "matchMedia", {
      configurable: true,
      value: (media: string) => ({
        matches: false,
        media,
        onchange: null,
        addEventListener() {},
        removeEventListener() {},
        addListener() {},
        removeListener() {},
        dispatchEvent: () => false,
      }),
    });
    // Anything the pane asks the host for on mount answers "nothing".
    (window as unknown as { zen: unknown }).zen = new Proxy(
      {
        getCapabilities: () => ({ supportsCloudSync: true }),
        getAppInfo: () => ({ runtime: "desktop" }),
      } as Record<string, unknown>,
      {
        get: (target, key: string) => target[key] ?? (async () => undefined),
      },
    );
    originalStore = useStore.getState();
    clearCloudSyncStatus();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });

  afterEach(() => {
    act(() => root.unmount());
    host.remove();
    clearCloudSyncStatus();
    useStore.setState(originalStore, true);
    delete (window as unknown as { zen?: unknown }).zen;
  });

  async function mountIn(mode: PaneMode): Promise<void> {
    useStore.setState({
      vault: { root: "/vault", name: "Vault" },
      activePaneId: pane.id,
      noteContents: { [note.path]: note },
      noteDirty: {},
      paneModes: { [pane.id]: { [note.path]: mode } },
      vimMode: false,
      keymapOverrides: {},
    });
    await act(async () => root.render(createElement(EditorPane, { pane })));
  }

  const banner = (): HTMLElement | null =>
    host.querySelector<HTMLElement>("[data-cloud-conflict-banner]");

  it.each<PaneMode>(["edit", "preview"])(
    "shows the banner above the note in %s mode only while the note waits",
    async (mode) => {
      await mountIn(mode);
      const editor = host.querySelector<HTMLElement>(".cm-editor");
      const preview = host.querySelector<HTMLElement>("[data-preview-scroll]");
      if (mode === "edit") {
        expect(editor).not.toBeNull();
        expect(preview).toBeNull();
      } else {
        expect(preview).not.toBeNull();
        expect(editor).not.toBeNull();
        expect(editor!.closest<HTMLElement>('[style*="display: none"]')).not.toBeNull();
      }
      expect(banner()).toBeNull();

      await act(async () => useCloudSyncStatusStore.setState({ lastSummary: waiting }));
      const shown = banner();
      expect(shown?.textContent).toContain("Sync is paused for this note");
      // Inside the pane's section, so its capture handlers still claim the
      // pane for a press on the banner, and ahead of the surface it covers.
      expect(shown?.closest(`[data-pane-id="${pane.id}"]`)).not.toBeNull();
      const surface = mode === "edit" ? editor : preview;
      expect(
        shown!.compareDocumentPosition(surface!) & Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
      // Arriving did not rebuild the surface underneath it.
      expect(host.querySelector(".cm-editor")).toBe(editor);

      await act(async () =>
        useCloudSyncStatusStore.setState({ lastSummary: { ...waiting, pending_conflicts: [] } }),
      );
      expect(banner()).toBeNull();
      expect(host.querySelector(".cm-editor")).toBe(editor);
    },
  );

  it("does not take the keyboard from the editor when it appears", async () => {
    await mountIn("edit");
    const content = host.querySelector<HTMLElement>(".cm-content")!;
    act(() => content.focus());
    expect(document.activeElement).toBe(content);

    await act(async () => useCloudSyncStatusStore.setState({ lastSummary: waiting }));
    expect(banner()).not.toBeNull();
    expect(document.activeElement).toBe(content);
  });

  it("lets a press on the banner claim the pane, like any press inside it", async () => {
    await mountIn("preview");
    await act(async () => useCloudSyncStatusStore.setState({ lastSummary: waiting }));
    const other: PaneLeaf = { ...pane, id: "pane-other", tabs: [], activeTab: null };
    act(() =>
      useStore.setState({
        paneLayout: {
          kind: "split",
          id: "split-1",
          direction: "row",
          children: [pane, other],
          sizes: [0.5, 0.5],
        },
        activePaneId: other.id,
        focusedPanel: "sidebar",
      }),
    );

    const review = host.querySelector<HTMLButtonElement>("[data-cloud-conflict-review]")!;
    const press = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    act(() => {
      review.dispatchEvent(press);
    });
    expect(press.defaultPrevented).toBe(true);
    expect(useStore.getState()).toMatchObject({
      activePaneId: pane.id,
      focusedPanel: "editor",
    });
  });

  it("leaves a note with nothing waiting alone while another note waits", async () => {
    await mountIn("edit");
    await act(async () =>
      useCloudSyncStatusStore.setState({
        lastSummary: {
          ...waiting,
          pending_conflicts: [{ ...waiting.pending_conflicts![0], path: "inbox/Other.md", cloud_path: null }],
        },
      }),
    );
    expect(banner()).toBeNull();
  });
});
