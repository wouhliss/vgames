// Hover/focus tooltip. Follows WCAG 1.4.13: appears on hover (after a short delay) and on keyboard
// focus, stays while hovered, and Escape dismisses it without moving focus.
import {
  cloneElement,
  isValidElement,
  type ReactElement,
  type ReactNode,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import styles from "./Tooltip.module.css";

const HOVER_DELAY_MS = 400;

export function Tooltip({
  content,
  children,
  placement = "bottom",
  describe = false,
}: {
  content: ReactNode;
  children: ReactElement;
  placement?: "top" | "bottom";
  /**
   * Link the tooltip as the trigger's description (aria-describedby). Leave false when the tooltip
   * repeats the trigger's accessible name, as for icon buttons.
   */
  describe?: boolean;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  useEffect(() => () => clearTimeout(timer.current), []);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpen(false);
      }
    }
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open]);

  const show = (delay: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(true), delay);
  };
  const hide = () => {
    clearTimeout(timer.current);
    setOpen(false);
  };

  const trigger =
    describe && isValidElement<{ "aria-describedby"?: string | undefined }>(children)
      ? cloneElement(children, { "aria-describedby": open ? id : undefined })
      : children;

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: the wrapper only observes hover and focus of its child.
    <span
      className={styles.anchor}
      onPointerEnter={() => show(HOVER_DELAY_MS)}
      onPointerLeave={hide}
      onFocus={(e) => {
        if (
          e.target.matches(":focus-visible") ||
          document.documentElement.dataset.modality !== "pointer"
        )
          show(0);
      }}
      onBlur={hide}
      onPointerDown={hide}
    >
      {trigger}
      {open ? (
        <span
          id={id}
          role="tooltip"
          className={`${styles.tooltip} ${styles[placement]}`}
          aria-hidden={describe ? undefined : true}
        >
          {content}
        </span>
      ) : null}
    </span>
  );
}
