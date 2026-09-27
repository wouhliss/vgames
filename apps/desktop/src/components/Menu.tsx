// Menu button and context menu (WAI-ARIA APG menu pattern). Items are real buttons with
// role="menuitem"; arrow keys (or the D-pad) move between them, Enter/A activates, Escape/B closes
// and returns focus. A context menu opens on right-click, Shift+F10, the ContextMenu key, or the
// controller's menu button on the focused element.
import {
  cloneElement,
  type KeyboardEvent,
  type MouseEvent,
  type ReactElement,
  type ReactNode,
  type Ref,
  type RefObject,
  useCallback,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { focusElement } from "../nav/focus";
import { useOptionalNav } from "../nav/NavProvider";
import { Icon, type IconName } from "./Icon";
import { popoverContainer } from "./layers";
import styles from "./Menu.module.css";
import { type Anchor, type PopoverPosition, placeBelow, useDismissPopover } from "./popover";

export type MenuEntry =
  | {
      id: string;
      label: string;
      onSelect: () => void;
      icon?: IconName | undefined;
      hint?: string | undefined;
      disabled?: boolean | undefined;
      danger?: boolean | undefined;
    }
  | { id: string; separator: true };

type Item = Extract<MenuEntry, { onSelect: () => void }>;

function isItem(entry: MenuEntry): entry is Item {
  return !("separator" in entry);
}

interface MenuPopupProps {
  id: string;
  label: string;
  entries: readonly MenuEntry[];
  anchor: Anchor;
  focusLast: boolean;
  onClose: (restoreFocus: boolean) => void;
  triggerRef: RefObject<HTMLElement | null>;
}

function MenuPopup({ id, label, entries, anchor, focusLast, onClose, triggerRef }: MenuPopupProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<PopoverPosition | null>(null);
  const typeahead = useRef({ text: "", at: 0 });
  const pushScope = useOptionalNav()?.pushScope;
  const close = useCallback(() => onClose(false), [onClose]);
  useDismissPopover(true, ref, triggerRef, close);

  const items = () =>
    Array.from(ref.current?.querySelectorAll<HTMLElement>("[role='menuitem']") ?? []);
  const enabledItems = () => items().filter((el) => el.getAttribute("aria-disabled") !== "true");

  // Position is computed once per opening, before the first paint.
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs once when the popup mounts.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setPosition(placeBelow(anchor, { width: el.offsetWidth, height: el.scrollHeight }, false));
    return pushScope?.(el);
  }, []);

  // Initial focus once the popup is positioned: while it is still `visibility: hidden`, browsers
  // refuse to focus anything inside it.
  const placed = position !== null;
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs once, when the popup becomes visible.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!placed || !el) return;
    const list = enabledItems();
    const first = focusLast ? list[list.length - 1] : list[0];
    focusElement(first ?? el);
  }, [placed]);

  const move = (delta: 1 | -1 | "first" | "last") => {
    const list = enabledItems();
    if (list.length === 0) return;
    const current = list.indexOf(document.activeElement as HTMLElement);
    let next: number;
    if (delta === "first") next = 0;
    else if (delta === "last") next = list.length - 1;
    else next = (current + delta + list.length) % list.length;
    const target = list[next];
    if (target) focusElement(target);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        move(1);
        return;
      case "ArrowUp":
        e.preventDefault();
        move(-1);
        return;
      case "Home":
        e.preventDefault();
        move("first");
        return;
      case "End":
        e.preventDefault();
        move("last");
        return;
      case "ArrowLeft":
      case "ArrowRight":
        e.preventDefault();
        return;
      case "Escape":
      case "ContextMenu":
        e.preventDefault();
        e.stopPropagation();
        onClose(true);
        return;
      case "Tab":
        e.preventDefault();
        onClose(true);
        return;
    }
    if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      const now = Date.now();
      const typing = now - typeahead.current.at < 700;
      // Space activates the focused item, unless it continues a typed search ("add to…").
      if (e.key === " " && !typing) return;
      e.preventDefault();
      const text = typing ? typeahead.current.text + e.key.toLowerCase() : e.key.toLowerCase();
      typeahead.current = { text, at: now };
      const match = enabledItems().find((el) =>
        el.textContent?.trim().toLowerCase().startsWith(text),
      );
      if (match) focusElement(match);
    }
  };

  return createPortal(
    <div
      ref={ref}
      id={id}
      role="menu"
      aria-label={label}
      data-popover=""
      tabIndex={-1}
      className={styles.menu}
      style={
        position
          ? { left: position.left, top: position.top, maxHeight: position.maxHeight }
          : { visibility: "hidden" }
      }
      onKeyDown={onKeyDown}
    >
      {entries.map((entry) =>
        isItem(entry) ? (
          <button
            key={entry.id}
            type="button"
            role="menuitem"
            tabIndex={-1}
            aria-disabled={entry.disabled || undefined}
            className={`${styles.item} ${entry.danger ? styles.danger : ""}`}
            onClick={() => {
              if (entry.disabled) return;
              onClose(true);
              entry.onSelect();
            }}
          >
            {entry.icon ? <Icon name={entry.icon} size={18} /> : null}
            <span className={styles.label}>{entry.label}</span>
            {entry.hint ? <span className={styles.hint}>{entry.hint}</span> : null}
          </button>
        ) : (
          <hr key={entry.id} className={styles.separator} />
        ),
      )}
    </div>,
    popoverContainer(triggerRef.current),
  );
}

type TriggerProps = {
  ref?: Ref<HTMLElement> | undefined;
  onClick?: ((e: MouseEvent) => void) | undefined;
  onKeyDown?: ((e: KeyboardEvent) => void) | undefined;
  "aria-haspopup"?: "menu" | undefined;
  "aria-expanded"?: boolean | undefined;
  "aria-controls"?: string | undefined;
};

/** A button that opens a menu. `trigger` must be a Button or IconButton element. */
export function Menu({
  trigger,
  label,
  entries,
}: {
  trigger: ReactElement<TriggerProps>;
  /** Accessible name of the menu itself, e.g. "Actions for Portal". */
  label: string;
  entries: readonly MenuEntry[];
}) {
  const id = useId();
  const triggerRef = useRef<HTMLElement | null>(null);
  const [open, setOpen] = useState<{ focusLast: boolean; anchor: Anchor } | null>(null);

  const openMenu = (focusLast: boolean) => {
    const el = triggerRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setOpen({ focusLast, anchor: { left: r.left, top: r.top, right: r.right, bottom: r.bottom } });
  };
  const onClose = useCallback((restoreFocus: boolean) => {
    setOpen(null);
    if (restoreFocus && triggerRef.current) focusElement(triggerRef.current);
  }, []);

  const wrapperRef = (node: HTMLSpanElement | null) => {
    triggerRef.current = node?.querySelector<HTMLElement>("button") ?? null;
  };

  return (
    <span ref={wrapperRef} className={styles.contextTarget}>
      {cloneElement(trigger, {
        "aria-haspopup": "menu",
        "aria-expanded": open !== null,
        "aria-controls": open ? id : undefined,
        onClick: (e: MouseEvent) => {
          trigger.props.onClick?.(e);
          if (open) setOpen(null);
          else openMenu(false);
        },
        onKeyDown: (e: KeyboardEvent) => {
          trigger.props.onKeyDown?.(e);
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            openMenu(e.key === "ArrowUp");
          }
        },
      })}
      {open ? (
        <MenuPopup
          id={id}
          label={label}
          entries={entries}
          anchor={open.anchor}
          focusLast={open.focusLast}
          onClose={onClose}
          triggerRef={triggerRef}
        />
      ) : null}
    </span>
  );
}

/**
 * Adds a context menu to its children. Right-click opens it at the pointer; Shift+F10, the
 * ContextMenu key and the controller menu button open it below the focused element.
 */
export function ContextMenu({
  label,
  entries,
  children,
}: {
  label: string;
  entries: readonly MenuEntry[];
  children: ReactNode;
}) {
  const id = useId();
  const returnTo = useRef<HTMLElement | null>(null);
  const [anchor, setAnchor] = useState<Anchor | null>(null);

  const onClose = useCallback((restoreFocus: boolean) => {
    setAnchor(null);
    if (restoreFocus && returnTo.current?.isConnected) focusElement(returnTo.current);
  }, []);

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: listens for the context-menu gesture on its focusable children.
    <div
      className={styles.contextTarget}
      onContextMenu={(e) => {
        e.preventDefault();
        returnTo.current =
          document.activeElement instanceof HTMLElement ? document.activeElement : null;
        setAnchor({ left: e.clientX, top: e.clientY, right: e.clientX, bottom: e.clientY });
      }}
      onKeyDown={(e) => {
        if (e.defaultPrevented) return;
        if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
          e.preventDefault();
          const target = e.target as HTMLElement;
          returnTo.current = target;
          const r = target.getBoundingClientRect();
          setAnchor({ left: r.left, top: r.top, right: r.right, bottom: r.bottom });
        }
      }}
    >
      {children}
      {anchor ? (
        <MenuPopup
          id={id}
          label={label}
          entries={entries}
          anchor={anchor}
          focusLast={false}
          onClose={onClose}
          triggerRef={returnTo}
        />
      ) : null}
    </div>
  );
}
