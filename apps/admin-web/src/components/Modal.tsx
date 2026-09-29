// A plain modal dialog: focus moves in on open and is trapped, Escape closes it (unless busy), and
// focus returns to whatever opened it. No animation, no library.
import { type KeyboardEvent, type ReactNode, useEffect, useId, useRef } from "react";

const TABBABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function Modal({
  title,
  children,
  onClose,
  busy = false,
  role = "dialog",
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  busy?: boolean;
  role?: "dialog" | "alertdialog";
}) {
  const panel = useRef<HTMLDivElement>(null);
  const titleId = useId();

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const el = panel.current;
    const first =
      el?.querySelector<HTMLElement>("[data-autofocus]") ??
      el?.querySelector<HTMLElement>(TABBABLE);
    (first ?? el)?.focus();
    return () => {
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      if (!busy) onClose();
      return;
    }
    if (e.key !== "Tab" || !panel.current) return;
    const items = [...panel.current.querySelectorAll<HTMLElement>(TABBABLE)];
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) return;
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: the layer only routes Escape and Tab for its dialog.
    <div className="modal-backdrop" onKeyDown={onKeyDown}>
      {/* biome-ignore lint/a11y/useAriaPropsSupportedByRole: role is always dialog or alertdialog, both modal. */}
      <div
        ref={panel}
        role={role}
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className="modal"
      >
        <h2 id={titleId}>{title}</h2>
        {children}
      </div>
    </div>
  );
}
