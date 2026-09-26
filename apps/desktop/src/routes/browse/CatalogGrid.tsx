// Virtualized catalog grid. Rows of cards are rendered only near the viewport (the shell's <main>
// scrolls); reaching the last rows asks for the next page.
import { useVirtualizer } from "@tanstack/react-virtual";
import { memo, type RefObject, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import { Badge } from "../../components/Feedback";
import { Icon } from "../../components/Icon";
import { t } from "../../i18n";
import type { Availability, CatalogItem, Platform } from "../../ipc";
import { columnsFor, GRID, initials, placeholderHue, tileWidth } from "../library/model";
import styles from "./Browse.module.css";

/** Title and platform line under the cover. */
const FOOTER = 52;
const LOADER_ROW = 64;

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

const scrollParent = (el: HTMLElement | null) => el?.closest<HTMLElement>("main") ?? null;

export function availabilityTone(availability: Availability) {
  return availability === "unavailable" ? "warning" : "neutral";
}

const FAMILIES = ["windows", "macos", "linux"] as const;

/** "Windows · Mac · Linux": operating systems with at least one build, whatever the CPU. */
export function platformLine(platforms: readonly Platform[]): string {
  return FAMILIES.filter((family) => platforms.some((p) => p.startsWith(`${family}-`)))
    .map((family) => t(`platformFamily.${family}`))
    .join(" · ");
}

const CatalogCard = memo(function CatalogCard({
  item,
  installed,
}: {
  item: CatalogItem;
  installed: boolean;
}) {
  const [broken, setBroken] = useState(false);
  const hue = placeholderHue(item.package_id);
  const id = item.package_id;
  return (
    <Link
      to={`/package/${id}`}
      className={styles.card}
      data-package={item.slug}
      aria-labelledby={`title-${id}`}
      aria-describedby={`meta-${id} badges-${id}`}
    >
      {item.cover_url && !broken ? (
        <img
          className={styles.cover}
          src={item.cover_url}
          alt=""
          loading="lazy"
          decoding="async"
          draggable={false}
          onError={() => setBroken(true)}
        />
      ) : (
        <span
          className={`${styles.cover} ${styles.placeholder}`}
          style={{
            background: `linear-gradient(160deg, hsl(${hue} 45% 32%), hsl(${(hue + 40) % 360} 55% 16%))`,
          }}
          aria-hidden="true"
        >
          {initials(item.title)}
        </span>
      )}
      <span id={`title-${id}`} className={styles.cardTitle}>
        {item.title}
      </span>
      <span id={`meta-${id}`} className={styles.cardMeta}>
        {platformLine(item.platforms)}
      </span>
      <span id={`badges-${id}`} className={styles.badges}>
        {installed ? (
          <Badge tone="success" icon="check">
            {t("browse.installed")}
          </Badge>
        ) : null}
        {item.availability !== "native" ? (
          <Badge tone={availabilityTone(item.availability)}>
            {t(`availability.${item.availability}`)}
          </Badge>
        ) : null}
      </span>
    </Link>
  );
});

export function CatalogGrid({
  items,
  installedIds,
  hasMore,
  loadingMore,
  loadMoreFailed,
  onLoadMore,
}: {
  items: readonly CatalogItem[];
  installedIds: ReadonlySet<string>;
  hasMore: boolean;
  loadingMore: boolean;
  loadMoreFailed: boolean;
  onLoadMore: () => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const width = useWidth(containerRef);
  const columns = columnsFor(width);
  const rows = useMemo(() => {
    const out: CatalogItem[][] = [];
    for (let i = 0; i < items.length; i += columns) out.push(items.slice(i, i + columns));
    return out;
  }, [items, columns]);
  const rowCount = rows.length + (hasMore || loadMoreFailed ? 1 : 0);
  const cardRow = Math.round(tileWidth(width, columns) * GRID.coverRatio) + FOOTER + GRID.gap;
  const [scrollMargin, setScrollMargin] = useState(0);

  useLayoutEffect(() => {
    const el = containerRef.current;
    const parent = scrollParent(el);
    if (!el || !parent) return;
    const margin =
      el.getBoundingClientRect().top - parent.getBoundingClientRect().top + parent.scrollTop;
    setScrollMargin((prev) => (Math.abs(prev - margin) > 0.5 ? margin : prev));
  });

  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollParent(containerRef.current),
    estimateSize: (index) => (index < rows.length ? cardRow : LOADER_ROW),
    overscan: 3,
    scrollMargin,
  });

  // biome-ignore lint/correctness/useExhaustiveDependencies: row heights depend on the width.
  useLayoutEffect(() => {
    virtualizer.measure();
  }, [virtualizer, cardRow]);

  const virtualItems = virtualizer.getVirtualItems();
  const lastIndex = virtualItems[virtualItems.length - 1]?.index ?? -1;
  // Ask for the next page when the last rows come into view.
  useEffect(() => {
    if (hasMore && !loadingMore && !loadMoreFailed && lastIndex >= rows.length - 2) onLoadMore();
  }, [hasMore, loadingMore, loadMoreFailed, lastIndex, rows.length, onLoadMore]);

  return (
    <div
      ref={containerRef}
      className={styles.virtual}
      style={{ height: virtualizer.getTotalSize() }}
      data-nav-group=""
      data-testid="catalog-grid"
    >
      {virtualItems.map((item) => {
        const row = rows[item.index];
        return (
          <div
            key={item.key}
            className={styles.virtualRow}
            style={{
              height: item.size,
              transform: `translateY(${item.start - virtualizer.options.scrollMargin}px)`,
            }}
          >
            {row ? (
              <div
                className={styles.row}
                style={{ gridTemplateColumns: `repeat(${columns}, 1fr)` }}
              >
                {row.map((entry) => (
                  <CatalogCard
                    key={entry.package_id}
                    item={entry}
                    installed={installedIds.has(entry.package_id)}
                  />
                ))}
              </div>
            ) : (
              <div className={styles.loader} role="status">
                {loadMoreFailed ? (
                  <button type="button" className={styles.retry} onClick={onLoadMore}>
                    <Icon name="refresh" size={16} />
                    {t("browse.loadMoreFailed")} {t("common.retry")}
                  </button>
                ) : (
                  t("browse.loadingMore")
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
