// Modal dialog: focus moves in on open, Tab is trapped, Escape (or B on a controller) closes it when
// dismissible, and focus returns to the element that opened it. Everything behind it is inert.
import {
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
} from "react";
import { createPortal } from "react-dom";
import { t } from "../i18n";
import { focusElement, tabbable } from "../nav/focus";
import { useOptionalNav } from "../nav/NavProvider";
import { IconButton } from "./Button";
import styles from "./Dialog.module.css";
import { isTopLayer, pushLayer } from "./layers";

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode | undefined;
  children?: ReactNode | undefined;
  footer?: ReactNode | undefined;
  size?: "sm" | "md" | "lg" | "xl" | undefined;
  /** When false, Escape, the backdrop and the close button do nothing (e.g. while saving). */
  dismissible?: boolean | undefined;
  /** Element to focus on open. Defaults to the first element with [data-autofocus], then the first tabbable one. */
  initialFocus?: RefObject<HTMLElement | null> | undefined;
  role?: "dialog" | "alertdialog" | undefined;
}

export function Dialog(props: DialogProps) {
  if (!props.open) return null;
  return <OpenDialog {...props} />;
}

function OpenDialog({
  onClose,
  title,
  description,
  children,
  footer,
  size = "md",
  dismissible = true,
  initialFocus,
  role = "dialog",
}: DialogProps) {
  const titleId = useId();
  const descId = useId();
  const layer = useRef<HTMLDivElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const nav = useOptionalNav();
  const pushScope = nav?.pushScope;

  // Remember what had focus, and give it back when the dialog goes away.
  useLayoutEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const layerEl = layer.current;
    const panelEl = panel.current;
    if (!layerEl || !panelEl) return;
    const popLayer = pushLayer(layerEl);
    const popScope = pushScope?.(panelEl);
    const target =
      initialFocus?.current ??
      panelEl.querySelector<HTMLElement>("[data-autofocus]") ??
      tabbable(panelEl)[0] ??
      panelEl;
    focusElement(target);
    return () => {
      popScope?.();
      popLayer();
      if (opener?.isConnected) focusElement(opener);
    };
  }, [initialFocus, pushScope]);

  // Keep focus inside even when something moves it programmatically.
  useEffect(() => {
    function onFocusIn(e: FocusEvent) {
      const panelEl = panel.current;
      if (!panelEl || !(e.target instanceof Node) || panelEl.contains(e.target)) return;
      // Popovers (menus, select lists) opened from inside the dialog live in their own portal.
      if (e.target instanceof Element && e.target.closest("[data-popover]")) return;
      const layerEl = layer.current;
      if (layerEl && isTopLayer(layerEl)) focusElement(tabbable(panelEl)[0] ?? panelEl);
    }
    document.addEventListener("focusin", onFocusIn);
    return () => document.removeEventListener("focusin", onFocusIn);
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (dismissible) onClose();
      return;
    }
    if (e.key !== "Tab" || !panel.current) return;
    const items = tabbable(panel.current);
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) {
      e.preventDefault();
      return;
    }
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      focusElement(last);
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      focusElement(first);
    }
  };

  return createPortal(
    // biome-ignore lint/a11y/noStaticElementInteractions: the layer only routes Escape and Tab for its dialog.
    <div ref={layer} className={styles.layer} onKeyDown={onKeyDown}>
      <div
        className={styles.backdrop}
        aria-hidden="true"
        onPointerDown={(e) => {
          if (e.target === e.currentTarget && dismissible) onClose();
        }}
      />
      {/* biome-ignore lint/a11y/useAriaPropsSupportedByRole: role is always dialog or alertdialog, both modal. */}
      <div
        ref={panel}
        role={role}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descId : undefined}
        tabIndex={-1}
        className={`${styles.dialog} ${styles[size]}`}
      >
        <div className={styles.header}>
          <div className={styles.titles}>
            <h2 id={titleId}>{title}</h2>
            {description ? (
              <div id={descId} className={styles.description}>
                {description}
              </div>
            ) : null}
          </div>
          {dismissible ? (
            <IconButton icon="close" label={t("common.close")} onClick={onClose} tooltip={false} />
          ) : null}
        </div>
        {children ? <div className={styles.body}>{children}</div> : null}
        {footer ? <div className={styles.footer}>{footer}</div> : null}
      </div>
    </div>,
    document.body,
  );
}
