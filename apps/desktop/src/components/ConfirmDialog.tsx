// Confirmation for consequential actions. The "typed" variant makes the user type a word (for
// example the package name) before the destructive button enables. For destructive actions the
// safe choice (Cancel) has focus first.
import { type ReactNode, useId, useRef, useState } from "react";
import { t } from "../i18n";
import { Button } from "./Button";
import { Dialog } from "./Dialog";
import { TextField } from "./TextField";

export interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description?: string;
  confirmLabel: string;
  cancelLabel?: string;
  tone?: "default" | "danger";
  /** Require typing this exact text (trimmed, case-sensitive) before confirming. */
  typedConfirmation?: string;
  /** Keeps the confirm button inert, e.g. while the dialog loads what it asks about. */
  confirmDisabled?: boolean;
  onConfirm: () => void | Promise<void>;
  onCancel: () => void;
  children?: ReactNode;
}

export function ConfirmDialog(props: ConfirmDialogProps) {
  if (!props.open) return null;
  return <OpenConfirm {...props} />;
}

function OpenConfirm({
  title,
  description,
  confirmLabel,
  cancelLabel,
  tone = "default",
  typedConfirmation,
  confirmDisabled = false,
  onConfirm,
  onCancel,
  children,
}: ConfirmDialogProps) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const inputId = useId();
  const matches =
    !confirmDisabled && (typedConfirmation === undefined || typed.trim() === typedConfirmation);

  const confirm = async () => {
    if (!matches || busy) return;
    setBusy(true);
    try {
      await onConfirm();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      role="alertdialog"
      size="sm"
      title={title}
      description={description}
      onClose={onCancel}
      dismissible={!busy}
      initialFocus={typedConfirmation === undefined && tone === "danger" ? cancelRef : undefined}
      footer={
        <>
          <Button ref={cancelRef} onClick={onCancel} aria-disabled={busy || undefined}>
            {cancelLabel ?? t("common.cancel")}
          </Button>
          <Button
            variant={tone === "danger" ? "danger" : "primary"}
            loading={busy}
            aria-disabled={!matches || undefined}
            onClick={() => void confirm()}
          >
            {confirmLabel}
          </Button>
        </>
      }
    >
      {children}
      {typedConfirmation !== undefined ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void confirm();
          }}
        >
          <TextField
            id={inputId}
            label={t("confirm.typeToConfirm", { text: typedConfirmation })}
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            data-autofocus=""
          />
        </form>
      ) : null}
    </Dialog>
  );
}
