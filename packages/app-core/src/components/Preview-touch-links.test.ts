// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { NoteMeta } from "@shared/ipc";
import { useStore } from "../store";
import { Preview } from "./Preview";

const navMocks = vi.hoisted(() => ({
  openWikilinkTarget: vi.fn(async () => true),
}));

vi.mock("../lib/wikilink-navigation", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/wikilink-navigation")>()),
  openWikilinkTarget: navMocks.openWikilinkTarget,
}));

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const notePath = "inbox/Heading link test.md";
const note = {
  path: notePath,
  title: "Heading link test",
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
  "# Heading link test",
  "",
  "Wikilink: [[#Section Three]]",
  "",
  "Markdown slug: [Go to section three](#section-three)",
  "",
  "## Section Three",
  "",
  "Target.",
].join("\n");

// The hover card renders the hovered note's body; this marks it in the page.
const CARD_MARKER = "HOVER CARD BODY";

async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
}

async function renderedLink(host: HTMLElement, selector: string): Promise<HTMLAnchorElement> {
  for (let i = 0; i < 50; i++) {
    const anchor = host.querySelector<HTMLAnchorElement>(selector);
    if (anchor) return anchor;
    await settle();
  }
  throw new Error(`preview never rendered ${selector}`);
}

/** jsdom has no PointerEvent; the handler only reads pointerType. */
function pointer(type: string, pointerType: string): Event {
  const event = new Event(type, { bubbles: true });
  Object.defineProperty(event, "pointerType", { value: pointerType });
  return event;
}

const mouse = (type: string): MouseEvent =>
  new MouseEvent(type, { bubbles: true, cancelable: true });

describe("Preview links on a touch screen", () => {
  let host: HTMLElement;
  let unmount: () => void;

  beforeEach(async () => {
    navMocks.openWikilinkTarget.mockClear();
    // jsdom has no CSS.escape; the in-page anchor lookup calls it first.
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
        readNote: async () => ({ ...note, body: `# Heading link test\n\n${CARD_MARKER}` }),
      },
    });
    useStore.setState({ notes: [note] });
    host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    act(() => root.render(createElement(Preview, { markdown, notePath })));
    unmount = () => {
      act(() => root.unmount());
      host.remove();
    };
  });

  afterEach(() => {
    unmount();
    delete (window as unknown as { zen?: unknown }).zen;
  });

  // Reported on iPhone 1.15.0: the tap opened a hover card of the note itself
  // and never scrolled. iOS drops the click of a tap whose mouseover changed
  // the page, so the card must not open from a touch.
  it("follows a tapped [[#Heading]] without opening a hover card", async () => {
    const anchor = await renderedLink(host, "a.wikilink");
    expect(anchor.dataset.resolvedPath).toBe(notePath);

    act(() => {
      anchor.dispatchEvent(pointer("pointerover", "touch"));
      anchor.dispatchEvent(pointer("pointerdown", "touch"));
      anchor.dispatchEvent(mouse("mouseover"));
      anchor.dispatchEvent(mouse("mousemove"));
    });
    await settle();
    expect(document.body.innerHTML).not.toContain(CARD_MARKER);

    act(() => {
      anchor.dispatchEvent(mouse("click"));
    });
    expect(navMocks.openWikilinkTarget).toHaveBeenCalledWith(notePath, "#Section Three");
  });

  it("still opens the hover card for a mouse", async () => {
    const anchor = await renderedLink(host, "a.wikilink");
    act(() => {
      anchor.dispatchEvent(pointer("pointerover", "mouse"));
      anchor.dispatchEvent(mouse("mouseover"));
    });
    await settle();
    await settle();
    expect(document.body.innerHTML).toContain(CARD_MARKER);
  });

  // Rendered headings carry no id, so the in-page anchor branch found nothing.
  it("follows a Markdown [text](#slug) link to the heading it names", async () => {
    const anchor = await renderedLink(host, 'a[href="#section-three"]');
    act(() => {
      anchor.dispatchEvent(mouse("click"));
    });
    expect(navMocks.openWikilinkTarget).toHaveBeenCalledWith(notePath, "#Section Three");
  });
});
