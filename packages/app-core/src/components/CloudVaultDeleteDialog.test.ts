import { describe, expect, it } from "vitest";
import { vaultNameConfirmed } from "./CloudVaultDeleteDialog";

describe("the typed vault name that enables Delete", () => {
  it("takes the name as a phone keyboard types it", () => {
    // iOS turns a typed apostrophe into a curly one.
    expect(vaultNameConfirmed("Adib’s notes", "Adib's notes")).toBe(true);
    expect(vaultNameConfirmed("Adib's notes", "Adib’s notes")).toBe(true);
    expect(vaultNameConfirmed(" Cloud QA iPhone ", "Cloud QA iPhone")).toBe(true);
  });

  it("asks for the whole name, in its own case", () => {
    expect(vaultNameConfirmed("Cloud QA", "Cloud QA iPhone")).toBe(false);
    expect(vaultNameConfirmed("cloud qa iphone", "Cloud QA iPhone")).toBe(false);
    expect(vaultNameConfirmed("", "Cloud QA iPhone")).toBe(false);
    expect(vaultNameConfirmed("", "")).toBe(false);
  });
});
