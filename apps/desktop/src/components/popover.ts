// Fixed-position popovers (menus, select listboxes) anchored to a rect, flipped to stay on screen.
import { type RefObject, useEffect } from "react";

export interface Anchor {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface PopoverPosition {
  left: number;
  top: number;
  minWidth: number;
  maxHeight: number;
}

const MARGIN = 8;

export function placeBelow(
  anchor: Anchor,
  size: { width: number; height: number },
  matchWidth: boolean,
): PopoverPosition {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const spaceBelow = vh - anchor.bottom - MARGIN;
  const spaceAbove = anchor.top - MARGIN;
  const above = size.height > spaceBelow && spaceAbove > spaceBelow;
  const maxHeight = Math.max(120, above ? spaceAbove - 4 : spaceBelow - 4);
  const height = Math.min(size.height, maxHeight);
  const top = above ? anchor.top - 4 - height : anchor.bottom + 4;
  const width = Math.max(size.width, matchWidth ? anchor.right - anchor.left : 0);
  const left = Math.min(Math.max(MARGIN, anchor.left), Math.max(MARGIN, vw - width - MARGIN));
  return { left, top, minWidth: matchWidth ? anchor.right - anchor.left : 0, maxHeight };
}

/** Closes a popover when the page scrolls (outside the popover), the window resizes, or the user clicks elsewhere. */
export function useDismissPopover(
  open: boolean,
  popover: RefObject<HTMLElement | null>,
  anchor: RefObject<HTMLElement | null>,
  close: () => void,
): void {
  useEffect(() => {
    if (!open) return;
    function onPointerDown(e: PointerEvent) {
      const target = e.target as Node;
      if (popover.current?.contains(target) || anchor.current?.contains(target)) return;
      close();
    }
    function onScroll(e: Event) {
      if (popover.current && e.target instanceof Node && popover.current.contains(e.target)) return;
      close();
    }
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
    };
  }, [open, popover, anchor, close]);
}
