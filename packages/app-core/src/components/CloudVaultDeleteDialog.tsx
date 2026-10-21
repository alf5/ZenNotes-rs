import { useId, useRef, useState } from "react";
import { isImeComposing } from "../lib/ime";
import { Button } from "./ui/Button";
import { Modal } from "./ui/Modal";

/**
 * Deleting a Cloud vault is the one Cloud action that reaches every device at
 * once. Its confirmation once looked so much like another device's that a
 * click on the wrong screen deleted the wrong vault, so this one names the
 * vault, says who it affects, and waits for its name to be typed.
 */
export function CloudVaultDeleteDialog({
  vaultName,
  onConfirm,
  onCancel,
}: {
  vaultName: string;
  onConfirm: () => void;
  onCancel: () => void;
}): JSX.Element {
  const [typed, setTyped] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const titleId = useId();
  const inputId = useId();
  const confirmed = vaultNameConfirmed(typed, vaultName);
  const confirm = (): void => {
    if (confirmed) onConfirm();
  };

  return (
    <Modal
      size="sm"
      layer="nested"
      onClose={onCancel}
      labelledBy={titleId}
      initialFocus={inputRef}
      data={{ "data-prompt-modal": "", "data-cloud-vault-delete-dialog": "" }}
    >
      {/* Settings renders this dialog, and React carries its keys up through
          the portal to Settings' own shortcuts (Mod+F would move focus to the
          settings search behind it). Tab still has to reach the shell's
          focus trap. */}
      <div
        onKeyDown={(event) => {
          if (event.key !== "Tab") event.stopPropagation();
        }}
      >
        <Modal.Header
          titleId={titleId}
          title={
            <span className="break-words">
              Delete “{vaultName}” for every device?
            </span>
          }
        />
        <div className="space-y-3 px-5 pt-2 text-sm leading-6 text-ink-600">
          <p>
            Every device linked to this Cloud vault stops syncing. Its Cloud
            copy, backups, and exports are permanently deleted, and this cannot
            be undone.
          </p>
          <p>
            Notes already on your devices stay where they are. To stop syncing
            on this device only, use Unlink this device instead.
          </p>
          <div>
            <label htmlFor={inputId} className="font-medium text-ink-800">
              To confirm, type{" "}
              <span className="break-words rounded-md bg-paper-200 px-1.5 py-0.5 font-semibold text-ink-900">
                {vaultName}
              </span>
            </label>
            <input
              id={inputId}
              ref={inputRef}
              value={typed}
              autoComplete="off"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              onChange={(event) => setTyped(event.target.value)}
              onKeyDown={(event) => {
                if (isImeComposing(event) || event.key !== "Enter") return;
                event.preventDefault();
                confirm();
              }}
              className="mt-2 w-full rounded-lg border border-paper-300 bg-paper-50 px-3 py-2 text-sm text-ink-900 outline-none focus:border-accent"
            />
          </div>
        </div>
        <Modal.Footer>
          <Button variant="secondary" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            variant="danger"
            disabled={!confirmed}
            data-cloud-vault-delete-confirm=""
            onClick={confirm}
          >
            Delete Cloud vault
          </Button>
        </Modal.Footer>
      </div>
    </Modal>
  );
}

/**
 * The name must match as typed, apart from what a keyboard does on its own: a
 * phone turns a typed apostrophe or quote into its curly form, and a stray
 * space at either end is not a different name.
 */
export function vaultNameConfirmed(typed: string, vaultName: string): boolean {
  const expected = comparableName(vaultName);
  return expected.length > 0 && comparableName(typed) === expected;
}

function comparableName(value: string): string {
  return value
    .normalize("NFC")
    .replace(/[‘’‛′]/g, "'")
    .replace(/[“”„″]/g, '"')
    .replace(/\s+/g, " ")
    .trim();
}
