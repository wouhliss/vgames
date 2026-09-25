// Virtualized library: only the rows near the viewport exist in the DOM, so 5,000 packages scroll
// like 50. Rows have exact heights computed from the width (no measuring, no layout shift). The
// page's scroll container is the shell's <main>, so keyboard, wheel and gamepad scrolling all work
// the same as on other screens.
import { useVirtualizer } from "@tanstack/react-virtual";
import { type RefObject, useLayoutEffect, useMemo, useRef, useState } from "react";
import { t } from "../../i18n";
import type { Library } from "../../ipc";
import styles from "./Library.module.css";
import {
  buildRows,
  columnsFor,
  GRID,
  isOffline,
  type Row,
  rowHeight,
  type Section,
  type ViewMode,
} from "./model";
import { GridTile, ListRow } from "./Tile";

function useWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.offsetWidth);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

function scrollParent(el: HTMLElement | null): HTMLElement | null {
  return el?.closest<HTMLElement>("main") ?? null;
}

export function LibraryView({
  sections,
  view,
  libraries,
}: {
  sections: readonly Section[];
  view: ViewMode;
  libraries: readonly Library[];
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const width = useWidth(containerRef);
  const columns = view === "grid" ? columnsFor(width) : 1;
  const rows = useMemo(() => buildRows(sections, columns), [sections, columns]);
  const [scrollMargin, setScrollMargin] = useState(0);

  // Distance from the top of the scroll container's content to this list.
  useLayoutEffect(() => {
    const el = containerRef.current;
    const parent = scrollParent(el);
    if (!el || !parent) return;
    const margin =
      el.getBoundingClientRect().top - parent.getBoundingClientRect().top + parent.scrollTop;
    setScrollMargin((prev) => (Math.abs(prev - margin) > 0.5 ? margin : prev));
  });

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollParent(containerRef.current),
    estimateSize: (index) => {
      const row = rows[index];
      return row ? rowHeight(row, view, width, columns) : GRID.listRow;
    },
    getItemKey: (index) => rows[index]?.key ?? index,
    overscan: 3,
    scrollMargin,
  });

  // Row heights depend on the width and layout; recompute every size when they change.
  // biome-ignore lint/correctness/useExhaustiveDependencies: the dependencies are the inputs of estimateSize.
  useLayoutEffect(() => {
    virtualizer.measure();
  }, [virtualizer, width, view, columns]);

  const offlineIds = useMemo(() => {
    const ids = new Set<string>();
    for (const section of sections)
      for (const pkg of section.items)
        if (isOffline(pkg, libraries)) ids.add(pkg.package.package_id);
    return ids;
  }, [sections, libraries]);

  const renderRow = (row: Row) => {
    if (row.kind === "header") {
      return (
        <h2 className={styles.sectionHeader}>
          {row.section === "favorites" ? t("library.sectionFavorites") : t("library.sectionOthers")}{" "}
          <span className={styles.sectionCount}>{t("library.count", { count: row.count })}</span>
        </h2>
      );
    }
    if (view === "list") {
      const pkg = row.items[0];
      return pkg ? <ListRow pkg={pkg} offline={offlineIds.has(pkg.package.package_id)} /> : null;
    }
    return (
      <div className={styles.gridRow} style={{ gridTemplateColumns: `repeat(${columns}, 1fr)` }}>
        {row.items.map((pkg) => (
          <GridTile
            key={pkg.package.package_id}
            pkg={pkg}
            offline={offlineIds.has(pkg.package.package_id)}
          />
        ))}
      </div>
    );
  };

  return (
    <div
      ref={containerRef}
      className={styles.virtual}
      style={{ height: virtualizer.getTotalSize() }}
      data-nav-group=""
      data-testid="library-view"
    >
      {virtualizer.getVirtualItems().map((item) => {
        const row = rows[item.index];
        if (!row) return null;
        return (
          <div
            key={item.key}
            className={styles.virtualRow}
            style={{
              height: item.size,
              transform: `translateY(${item.start - virtualizer.options.scrollMargin}px)`,
            }}
          >
            {renderRow(row)}
          </div>
        );
      })}
    </div>
  );
}
