// Hover/focus tooltip. Follows WCAG 1.4.13: appears on hover (after a short delay) and on keyboard
// focus, stays while hovered, and Escape dismisses it without moving focus.
import {
  cloneElement,
  isValidElement,
  type ReactElement,
  type ReactNode,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import styles from "./Tooltip.module.css";

const HOVER_DELAY_MS = 400;
const EDGE = 4;

/** Left and right edges of the area `el` is visible in: the viewport, cut by scrolling ancestors. */
function visibleSpan(el: HTMLElement): [number, number] {
  let min = 0;
  let max = document.documentElement.clientWidth;
  for (let node = el.parentElement; node; node = node.parentElement) {
    const style = getComputedStyle(node);
    // Browsers resolve overflow-x from the shorthand; test DOMs may only keep the shorthand.
    if (style.overflowX === "visible" && (style.overflow.split(" ")[0] || "visible") === "visible")
      continue;
    const rect = node.getBoundingClientRect();
    min = Math.max(min, rect.left);
    max = Math.min(max, rect.right);
  }
  return [min, max];
}

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
  const [shift, setShift] = useState(0);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const tip = useRef<HTMLSpanElement>(null);

  // Centered on the trigger, but moved sideways so a dialog's or panel's edge never cuts it off.
  // Measured only while open: closed tooltips (one per tile in long lists) cost nothing.
  useLayoutEffect(() => {
    const el = tip.current;
    if (!open || !el) return;
    const rect = el.getBoundingClientRect();
    const [min, max] = visibleSpan(el);
    if (rect.left < min + EDGE) setShift(min + EDGE - rect.left);
    else if (rect.right > max - EDGE) setShift(max - EDGE - rect.right);
  }, [open]);

  useEffect(() => () => clearTimeout(timer.current), []);

  // Registered while committing, not after paint: Escape must work from the moment the tooltip shows.
  useLayoutEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpen(false);
        setShift(0);
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
    setShift(0);
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
          ref={tip}
          id={id}
          role="tooltip"
          className={`${styles.tooltip} ${styles[placement]}`}
          style={shift ? { translate: `calc(-50% + ${Math.round(shift)}px) 0` } : undefined}
          aria-hidden={describe ? undefined : true}
        >
          {content}
        </span>
      ) : null}
    </span>
  );
}
