import { useEffect, useState } from "react";
import {
  describeCloudError,
  type CloudErrorCopy,
} from "@zennotes/shared-domain/cloud-error-copy";
import { writeClipboardText } from "../lib/clipboard-text";
import { signOutAfterRefusal } from "../lib/cloud-auto-sync";
import { Button } from "./ui/Button";

const APP_ISSUES_URL = "https://github.com/ZenNotes/zennotes/issues";

/**
 * A failed Cloud action in words a person can act on. The desktop main
 * process words its errors before IPC and appends the technical line; the
 * phones' in-process hosts hand over the error itself. Either way the result
 * is the sentence to show and, apart from it, the line support needs.
 */
export function cloudErrorCopy(
  error: unknown,
  fallback: string,
  /** Given, a refused sign-in also signs this device out, as sync does. */
  bridge?: Parameters<typeof signOutAfterRefusal>[0],
): CloudErrorCopy {
  const copy = describeCloudError(error, fallback);
  if (bridge && copy.kind === "signed-out") signOutAfterRefusal(bridge, copy);
  return copy;
}

/**
 * The sentence, and under it a closed Details disclosure with the technical
 * line, Copy details, and Report an Issue (the same words as the Help menu
 * and the command palette). The surrounding alert box belongs to the caller.
 */
export function CloudErrorMessage({
  message,
  detail,
}: {
  message: string;
  detail: string | null;
}): JSX.Element {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1500);
    return () => window.clearTimeout(timer);
  }, [copied]);

  return (
    <>
      <span data-cloud-error-message="">{message}</span>
      {detail && (
        <details data-cloud-error-details="" className="mt-2 text-ink-600">
          <summary className="cursor-pointer text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40">
            Details
          </summary>
          <pre className="mt-2 whitespace-pre-wrap break-words rounded-lg border border-paper-300/70 bg-paper-100/70 px-3 py-2 font-mono text-xs leading-5 text-ink-600">
            {detail}
          </pre>
          <div className="mt-2 flex flex-wrap gap-2">
            <Button
              variant="ghost"
              aria-label="Copy error details"
              onClick={() => setCopied(writeClipboardText(detail))}
            >
              {copied ? "Copied" : "Copy details"}
            </Button>
            <Button
              variant="ghost"
              onClick={() => window.open(APP_ISSUES_URL, "_blank")}
            >
              Report an Issue
            </Button>
          </div>
        </details>
      )}
    </>
  );
}
