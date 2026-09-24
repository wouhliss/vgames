// DOM helpers shared by the spatial navigation layer, focus traps and menus.

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
  "[contenteditable='true']",
].join(",");

function isHidden(el: Element): boolean {
  return el.closest("[hidden], [inert], [aria-hidden='true']") !== null;
}

/** Elements reachable with Tab inside `root`, in DOM order. */
export function tabbable(root: ParentNode): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => !isHidden(el) && el.tabIndex >= 0 && !el.hasAttribute("data-nav-skip"),
  );
}

export function isTextEntry(el: Element | null): el is HTMLInputElement | HTMLTextAreaElement {
  if (el instanceof HTMLTextAreaElement) return true;
  if (!(el instanceof HTMLInputElement)) return false;
  return ![
    "button",
    "checkbox",
    "radio",
    "range",
    "color",
    "file",
    "submit",
    "reset",
    "image",
  ].includes(el.type);
}

/** Focus without the browser's jump-scroll, then bring the element into view with minimal movement. */
export function focusElement(el: HTMLElement): void {
  el.focus({ preventScroll: true });
  el.scrollIntoView?.({ block: "nearest", inline: "nearest" });
}
