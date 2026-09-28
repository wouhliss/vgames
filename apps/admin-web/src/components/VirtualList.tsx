// A virtualized list for very long lists (100k files stay responsive): only rows near the viewport
// are in the DOM. Keyboard users scroll it as a focusable region.
import { useVirtualizer } from "@tanstack/react-virtual";
import { type ReactNode, useRef } from "react";

/** The scrolling list must be reachable with the keyboard (axe: scrollable-region-focusable). */
const SCROLLABLE = { tabIndex: 0 } as const;

export function VirtualList<T>({
  items,
  label,
  render,
  rowHeight = 26,
  height = 280,
}: {
  items: readonly T[];
  label: string;
  render: (item: T, index: number) => ReactNode;
  rowHeight?: number;
  height?: number;
}) {
  const parent = useRef<HTMLElement>(null);
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => parent.current,
    estimateSize: () => rowHeight,
    overscan: 10,
    initialRect: { width: 600, height },
  });
  return (
    <section
      ref={parent}
      className="virtual-list"
      style={{ height }}
      aria-label={label}
      {...SCROLLABLE}
    >
      <ul style={{ height: virtualizer.getTotalSize() }} aria-label={label}>
        {virtualizer.getVirtualItems().map((row) => {
          const item = items[row.index];
          return item === undefined ? null : (
            <li
              key={row.key}
              aria-setsize={items.length}
              aria-posinset={row.index + 1}
              style={{ height: rowHeight, transform: `translateY(${row.start}px)` }}
            >
              {render(item, row.index)}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
