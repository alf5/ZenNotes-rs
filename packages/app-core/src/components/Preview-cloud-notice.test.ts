// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { NoteMeta } from "@shared/ipc";
import type {
  CloudSyncConflict,
  CloudSyncRunSummary,
} from "@zennotes/bridge-contract/cloud-sync";
import {
  clearCloudSyncStatus,
  useCloudSyncStatusStore,
} from "../lib/cloud-auto-sync";
import { useStore } from "../store";
import { Preview } from "./Preview";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const notePath = "inbox/Media.md";
const note = {
  path: notePath,
  title: "Media",
  folder: "inbox",
  siblingOrder: 0,
  createdAt: 0,
  updatedAt: 0,
  size: 0,
  tags: [],
  wikilinks: [],
  assetEmbeds: [],
  hasAttachments: false,
  excerpt: "",
} as NoteMeta;

const markdown = [
  "# Media",
  "",
  "![[assets/clip.mp4]]",
  "",
  "![[assets/song.mp3]]",
  "",
  "![[assets/pic.png]]",
  "",
  "![[assets/doc.pdf]]",
  "",
  "![](assets/archive.zip)",
  "",
  "![](assets/other.mp4)",
].join("\n");

const NOTICE =
  "Not synced to Cloud: larger than the 10 MB file-size limit, so it stays on this device.";

function tooLarge(path: string): CloudSyncConflict {
  return {
    operation_id: `op-${path}`,
    item_id: `item-${path}`,
    code: "FILE_SIZE_LIMIT_EXCEEDED",
    current_revision: null,
    current_path: null,
    path,
    capacity: {
      dimension: "sync_max_file_bytes",
      used: 0,
      reserved: 0,
      limit: 10_000_000,
      projected: 30_000_000,
      can_retry_after_reduction: true,
    },
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

async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
}

async function waitFor(check: () => boolean, what: string): Promise<void> {
  for (let i = 0; i < 50; i++) {
    if (check()) return;
    await settle();
  }
  throw new Error(`preview never showed ${what}`);
}

function embed(host: HTMLElement, href: string): HTMLElement | null {
  return host.querySelector<HTMLElement>(
    `[data-local-asset-href="${href}"]:not(a)`,
  );
}

function noticeHosts(host: HTMLElement): string[] {
  return [...host.querySelectorAll("[data-cloud-sync-notice]")].map(
    (el) => (el.parentElement as HTMLElement).dataset.localAssetHref ?? "",
  );
}

/** Renders `body` in the reading view over a vault that holds `assetPaths`. */
function mountPreview(
  body: string,
  assetPaths: string[],
  options: { listed?: boolean; onRequestEdit?: () => void } = {},
): { host: HTMLElement; unmount: () => void } {
  if (!globalThis.CSS?.escape) {
    vi.stubGlobal("CSS", { escape: (value: string) => value.replace(/[^\w-]/g, "\\$&") });
  }
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: vi.fn().mockReturnValue({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }),
  });
  Object.defineProperty(window, "zen", {
    configurable: true,
    value: {
      getAppInfo: () => ({ runtime: "web" }),
      readNote: async () => ({ ...note, body }),
      resolveVaultAssetUrl: (_root: string, rel: string) => `zen-asset://vault/${rel}`,
      resolveLocalAssetUrl: (_root: string, _note: string, href: string) =>
        `zen-asset://note/${href}`,
    },
  });
  useStore.setState({
    notes: [note],
    vault: { root: "/vault", name: "Vault" } as never,
    assetFiles: assetPaths.map((path) => ({ path })) as never,
    assetFilesListed: options.listed ?? false,
  });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() =>
    root.render(
      createElement(Preview, {
        markdown: body,
        notePath,
        onRequestEdit: options.onRequestEdit ?? null,
      }),
    ),
  );
  return {
    host,
    unmount: () => {
      act(() => root.unmount());
      host.remove();
    },
  };
}

describe("Preview: embeds Cloud keeps on this device", () => {
  let host: HTMLElement;
  let unmount: () => void;

  beforeEach(async () => {
    clearCloudSyncStatus();
    ({ host, unmount } = mountPreview(markdown, [
      "assets/clip.mp4",
      "assets/song.mp3",
      "assets/pic.png",
      "assets/doc.pdf",
      "assets/archive.zip",
      "assets/other.mp4",
    ]));
    await waitFor(() => embed(host, "assets/clip.mp4") !== null, "the media");
  });

  afterEach(() => {
    unmount();
    clearCloudSyncStatus();
    useStore.setState({ notes: [], vault: null, assetFiles: [] });
    delete (window as unknown as { zen?: unknown }).zen;
  });

  it("plays ![](clip.mp4) where it stands, as it plays ![[clip.mp4]]", () => {
    for (const href of ["assets/clip.mp4", "assets/other.mp4"]) {
      const figure = embed(host, href)!;
      expect(figure.classList.contains("local-asset-embed")).toBe(true);
      expect(figure.dataset.localAssetKind).toBe("video");
      expect(figure.querySelector("video")?.getAttribute("src")).toBe(
        `zen-asset://vault/${href}`,
      );
    }
    // A file with nothing to play is still a chip.
    expect(embed(host, "assets/archive.zip")?.classList.contains("local-file-attachment")).toBe(true);
  });

  it("says so under the oversized embed only, and drops it when the summary clears", async () => {
    act(() => {
      useCloudSyncStatusStore.setState({ lastSummary: run(tooLarge("assets/clip.mp4")) });
    });
    await waitFor(() => noticeHosts(host).length > 0, "the notice");

    expect(noticeHosts(host)).toEqual(["assets/clip.mp4"]);
    const notice = host.querySelector("[data-cloud-sync-notice]")!;
    expect(notice.textContent).toBe(NOTICE);
    expect(notice.previousElementSibling?.tagName).toBe("VIDEO");

    act(() => {
      useCloudSyncStatusStore.setState({ lastSummary: null });
    });
    await waitFor(() => noticeHosts(host).length === 0, "the notice gone");
    expect(embed(host, "assets/clip.mp4")?.querySelector("video")).not.toBeNull();
  });

  it("is on image, video, audio, PDF and attachment-chip embeds alike", async () => {
    act(() => {
      useCloudSyncStatusStore.setState({
        lastSummary: run(
          tooLarge("assets/clip.mp4"),
          tooLarge("assets/song.mp3"),
          tooLarge("assets/pic.png"),
          tooLarge("assets/doc.pdf"),
          tooLarge("assets/archive.zip"),
          tooLarge("assets/other.mp4"),
        ),
      });
    });
    await waitFor(() => noticeHosts(host).length === 6, "six notices");

    expect(noticeHosts(host)).toEqual([
      "assets/clip.mp4",
      "assets/song.mp3",
      "assets/pic.png",
      "assets/doc.pdf",
      "assets/archive.zip",
      "assets/other.mp4",
    ]);
    const kinds = [...host.querySelectorAll("[data-cloud-sync-notice]")].map(
      (el) => (el.parentElement as HTMLElement).dataset.localAssetKind,
    );
    expect(kinds).toEqual(["video", "audio", "image", "pdf", "file", "video"]);
  });

  it("does not render the note again for a run that leaves no file over the limit", async () => {
    const video = embed(host, "assets/clip.mp4")!.querySelector("video");
    act(() => {
      useCloudSyncStatusStore.setState({ lastSummary: run() });
    });
    act(() => {
      useCloudSyncStatusStore.setState({
        lastSummary: run({
          operation_id: "op-rev",
          item_id: "rev",
          code: "REVISION_CONFLICT",
          current_revision: 2,
          current_path: null,
          path: notePath,
        }),
      });
    });
    await settle();
    await settle();

    expect(noticeHosts(host)).toEqual([]);
    expect(embed(host, "assets/clip.mp4")!.querySelector("video")).toBe(video);
  });
});

// A file over Cloud's per-file limit stays on the device that has it. Every
// other device gets the note, its embed, and no file behind the embed.
describe("Preview: embeds whose file is not on this device", () => {
  const body = [
    "# Media",
    "",
    "![[assets/clip.mp4]]",
    "",
    "![[assets/gone.mp4]]",
    "",
    "![[assets/gone.mp3]]",
    "",
    "![[assets/gone.png]]",
    "",
    "![[assets/gone.pdf]]",
    "",
    "![](assets/gone%20too.mp4)",
    "",
    "[The spec](assets/spec.pdf)",
    "",
    "![](assets/scan.pdf)",
    "",
    "![[assets/gone.zip]]",
  ].join("\n");

  let mounted: { host: HTMLElement; unmount: () => void } | null = null;

  afterEach(() => {
    mounted?.unmount();
    mounted = null;
    useStore.setState({ notes: [], vault: null, assetFiles: [], assetFilesListed: false });
    delete (window as unknown as { zen?: unknown }).zen;
  });

  function missing(host: HTMLElement): Array<[string, string, string]> {
    return [...host.querySelectorAll<HTMLElement>("[data-local-asset-missing]")].map((card) => [
      card.dataset.localAssetMissing ?? "",
      card.dataset.localAssetHref ?? "",
      card.querySelector('[role="note"]')?.textContent ?? "",
    ]);
  }

  it("says so in place of the player, picture or document, and plays the file that is here", async () => {
    mounted = mountPreview(body, ["assets/clip.mp4"], { listed: true });
    const { host } = mounted;
    await waitFor(() => missing(host).length > 0, "the notices");

    expect(missing(host)).toEqual([
      ["video", "assets/gone.mp4", "gone.mp4 isn't on this device."],
      ["audio", "assets/gone.mp3", "gone.mp3 isn't on this device."],
      ["image", "assets/gone.png", "gone.png isn't on this device."],
      ["pdf", "assets/gone.pdf", "gone.pdf isn't on this device."],
      ["video", "assets/gone%20too.mp4", "gone too.mp4 isn't on this device."],
      ["pdf", "assets/spec.pdf", "spec.pdf isn't on this device."],
      ["pdf", "assets/scan.pdf", "scan.pdf isn't on this device."],
    ]);
    for (const card of host.querySelectorAll("[data-local-asset-missing]")) {
      expect(card.querySelector("video, audio, img, iframe, a, button")).toBeNull();
      expect(card.querySelector('[role="note"]')?.classList.contains("text-warning")).toBe(true);
    }
    expect(embed(host, "assets/clip.mp4")?.querySelector("video")?.getAttribute("src")).toBe(
      "zen-asset://vault/assets/clip.mp4",
    );
    // The listing cannot vouch for every file a chip names, so chips stay.
    expect(embed(host, "assets/gone.zip")?.classList.contains("local-file-attachment")).toBe(true);
  });

  it("keeps its block's source line, so a double-click opens that line in the editor", async () => {
    const onRequestEdit = vi.fn();
    mounted = mountPreview(body, ["assets/clip.mp4"], { listed: true, onRequestEdit });
    const { host } = mounted;
    await waitFor(() => missing(host).length > 0, "the notices");

    const card = embed(host, "assets/gone.mp4")!;
    expect(card.dataset.sourceLine).toBe("5");
    card
      .querySelector('[role="note"]')!
      .dispatchEvent(new MouseEvent("dblclick", { bubbles: true, button: 0 }));
    expect(onRequestEdit).toHaveBeenCalledWith(expect.objectContaining({ sourceLine: 5 }));
  });

  it("says so in a vault whose listing holds no files at all, once that listing lands", async () => {
    mounted = mountPreview("![[assets/gone.mp4]]\n\n![[assets/gone.png]]", [], {
      listed: false,
    });
    const { host } = mounted;
    await settle();
    await settle();
    // Not listed yet: no file can be called missing on an empty list.
    expect(missing(host)).toEqual([]);

    act(() => {
      useStore.setState({ assetFiles: [], assetFilesListed: true });
    });
    await waitFor(() => missing(host).length === 2, "two notices");
    expect(host.querySelector("video, img")).toBeNull();
  });

  it("gives way to the player once the file arrives", async () => {
    mounted = mountPreview("![[assets/gone.mp4]]", ["assets/clip.mp4"], { listed: true });
    const { host } = mounted;
    await waitFor(() => missing(host).length === 1, "the notice");

    act(() => {
      useStore.setState({
        assetFiles: [{ path: "assets/clip.mp4" }, { path: "assets/gone.mp4" }] as never,
      });
    });
    await waitFor(() => missing(host).length === 0, "the notice gone");
    expect(embed(host, "assets/gone.mp4")?.querySelector("video")?.getAttribute("src")).toBe(
      "zen-asset://vault/assets/gone.mp4",
    );
  });

  it("trusts only a listing read from the vault: a list seeded by hand keeps its players", async () => {
    // The share viewer seeds the store with the files its page carries and
    // serves the rest from the guessed URL.
    mounted = mountPreview("![[assets/gone.mp4]]", ["assets/clip.mp4"], { listed: false });
    const { host } = mounted;
    await waitFor(() => embed(host, "assets/gone.mp4") !== null, "the player");

    expect(missing(host)).toEqual([]);
    expect(embed(host, "assets/gone.mp4")?.querySelector("video")?.getAttribute("src")).toBe(
      "zen-asset://note/assets/gone.mp4",
    );
  });
});
