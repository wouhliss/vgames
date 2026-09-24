// Modal layers. While a modal layer (dialog, trust block) is open, everything else in <body> is
// made `inert`, so neither Tab, the pointer, the D-pad nor assistive technology can reach it.
// Layers stack: only the top one stays interactive.

const stack: HTMLElement[] = [];
const madeInert = new Set<Element>();

function apply(): void {
  const top = stack[stack.length - 1];
  for (const el of madeInert) {
    if (!top || el === top || el.contains(top)) {
      el.removeAttribute("inert");
      madeInert.delete(el);
    }
  }
  if (!top) return;
  for (const child of Array.from(document.body.children)) {
    if (child === top || child.contains(top) || child.tagName === "SCRIPT") continue;
    if (!child.hasAttribute("inert")) {
      child.setAttribute("inert", "");
      madeInert.add(child);
    }
  }
}

export function pushLayer(el: HTMLElement): () => void {
  stack.push(el);
  apply();
  return () => {
    const index = stack.indexOf(el);
    if (index >= 0) stack.splice(index, 1);
    apply();
  };
}

export function isTopLayer(el: HTMLElement): boolean {
  return stack[stack.length - 1] === el;
}
