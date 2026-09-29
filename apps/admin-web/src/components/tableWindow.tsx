// Table rows for lists that grow page by page to 10k entries and more (A3-T18). Up to `THRESHOLD`
// rows every row is rendered (find-in-page works); above it only the rows near the scroll position
// of the `.table-wrap` are, between two spacer rows, so the table keeps its semantics, its sticky
// header and its keyboard order. Rows are measured, so wrapping titles don't drift.
import { useVirtualizer } from "@tanstack/react-virtual";
import { useRef } from "react";

export const THRESHOLD = 200;

export interface TableWindow<T> {
  /** Goes on the scrolling `.table-wrap`. */
  scrollRef: React.RefObject<HTMLDivElement | null>;
  /** The rows to render now, with their index in `items`. */
  rows: { item: T; index: number }[];
  /** Goes on each rendered `<tr>` (with `data-index`), to measure it. */
  measure: ((row: HTMLTableRowElement | null) => void) | undefined;
  /** Space for the rows above and below the rendered ones, in pixels. */
  before: number;
  after: number;
}

export function useTableWindow<T>(items: readonly T[], rowHeight = 37): TableWindow<T> {
  const scrollRef = useRef<HTMLDivElement>(null);
  const windowed = items.length > THRESHOLD;
  const virtualizer = useVirtualizer({
    count: windowed ? items.length : 0,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 15,
    initialRect: { width: 800, height: 600 },
  });
  if (!windowed) {
    return {
      scrollRef,
      rows: items.map((item, index) => ({ item, index })),
      measure: undefined,
      before: 0,
      after: 0,
    };
  }
  const visible = virtualizer.getVirtualItems();
  const rows: { item: T; index: number }[] = [];
  for (const v of visible) {
    const item = items[v.index];
    if (item !== undefined) rows.push({ item, index: v.index });
  }
  const first = visible[0];
  const last = visible[visible.length - 1];
  return {
    scrollRef,
    rows,
    measure: virtualizer.measureElement,
    before: first ? first.start : 0,
    after: last ? Math.max(0, virtualizer.getTotalSize() - last.end) : 0,
  };
}

/** Keeps the scroll height of the rows that aren't rendered. */
export function SpacerRow({ height, columns }: { height: number; columns: number }) {
  if (height <= 0) return null;
  return (
    <tr aria-hidden="true" className="spacer" style={{ height }}>
      <td colSpan={columns} />
    </tr>
  );
}
