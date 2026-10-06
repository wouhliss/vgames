// Pure library logic: filtering, sorting, sections and the rows the virtualized view renders.
import { currentLocale } from "../../i18n";
import type { InstalledPackage, LibraryInfo } from "../../ipc";

export type SortKey = "recent" | "name" | "size" | "installed";
export type ViewMode = "grid" | "list";
/** "all", "favorites", or `c:<collection id>`. */
export type FilterValue = "all" | "favorites" | `c:${string}`;

/** Lowercase, without diacritics, so "okami" finds "Ōkami". */
export function searchKey(text: string): string {
  return text.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase();
}

export function matchesFilter(pkg: InstalledPackage, filter: FilterValue): boolean {
  if (filter === "all") return true;
  if (filter === "favorites") return pkg.favorite;
  return pkg.collection_ids.includes(filter.slice(2));
}

export function filterInstalls(
  items: readonly InstalledPackage[],
  query: string,
  filter: FilterValue,
): InstalledPackage[] {
  const needle = searchKey(query.trim());
  return items.filter(
    (pkg) => matchesFilter(pkg, filter) && (needle === "" || searchKey(pkg.title).includes(needle)),
  );
}

function timeOf(iso: string | null): number {
  if (iso === null) return Number.NEGATIVE_INFINITY;
  const time = Date.parse(iso);
  return Number.isNaN(time) ? Number.NEGATIVE_INFINITY : time;
}

export function sortInstalls(items: readonly InstalledPackage[], key: SortKey): InstalledPackage[] {
  const collator = new Intl.Collator(currentLocale(), { sensitivity: "base", numeric: true });
  const byName = (a: InstalledPackage, b: InstalledPackage) => collator.compare(a.title, b.title);
  const compare: Record<SortKey, (a: InstalledPackage, b: InstalledPackage) => number> = {
    name: byName,
    // Never-played packages go last, newest installs first among them.
    recent: (a, b) =>
      timeOf(b.last_played_at) - timeOf(a.last_played_at) ||
      timeOf(b.installed_at) - timeOf(a.installed_at) ||
      byName(a, b),
    size: (a, b) => b.size_bytes - a.size_bytes || byName(a, b),
    installed: (a, b) => timeOf(b.installed_at) - timeOf(a.installed_at) || byName(a, b),
  };
  return [...items].sort(compare[key]);
}

export type SectionId = "favorites" | "others";

export interface Section {
  id: SectionId;
  items: InstalledPackage[];
}

/** Favorites are pinned at the top. Without favorites (or in the Favorites view) there is one section. */
export function sectionsOf(sorted: readonly InstalledPackage[], filter: FilterValue): Section[] {
  if (filter === "favorites") return [{ id: "favorites", items: [...sorted] }];
  const favorites = sorted.filter((p) => p.favorite);
  const others = sorted.filter((p) => !p.favorite);
  if (favorites.length === 0) return [{ id: "others", items: others }];
  if (others.length === 0) return [{ id: "favorites", items: favorites }];
  return [
    { id: "favorites", items: favorites },
    { id: "others", items: others },
  ];
}

export type Row =
  | { kind: "header"; key: string; section: SectionId; count: number }
  | { kind: "items"; key: string; section: SectionId; items: InstalledPackage[] };

/** Flattens sections into virtual rows of `columns` tiles. Headers only when there are two sections. */
export function buildRows(sections: readonly Section[], columns: number): Row[] {
  const perRow = Math.max(1, Math.floor(columns));
  const rows: Row[] = [];
  const headers = sections.length > 1;
  for (const section of sections) {
    if (headers)
      rows.push({
        kind: "header",
        key: `h-${section.id}`,
        section: section.id,
        count: section.items.length,
      });
    for (let i = 0; i < section.items.length; i += perRow) {
      const items = section.items.slice(i, i + perRow);
      rows.push({
        kind: "items",
        key: `${section.id}-${items[0]?.package.package_id ?? i}`,
        section: section.id,
        items,
      });
    }
  }
  return rows;
}

export const GRID = {
  minTileWidth: 150,
  gap: 16,
  /** Title, meta line and action buttons under the cover. */
  footer: 96,
  /** Cover height / width (2:3 portrait capsules). */
  coverRatio: 1.5,
  header: 48,
  listRow: 64,
} as const;

export function columnsFor(width: number): number {
  if (width <= 0) return 1;
  return Math.max(1, Math.floor((width + GRID.gap) / (GRID.minTileWidth + GRID.gap)));
}

export function tileWidth(width: number, columns: number): number {
  return Math.max(GRID.minTileWidth, (width - GRID.gap * (columns - 1)) / columns);
}

/** Exact row heights (no measuring), so scrolling never shifts. */
export function rowHeight(row: Row, view: ViewMode, width: number, columns: number): number {
  if (row.kind === "header") return GRID.header;
  if (view === "list") return GRID.listRow;
  return Math.round(tileWidth(width, columns) * GRID.coverRatio) + GRID.footer + GRID.gap;
}

/** Whether the package's library drive is missing. */
export function isOffline(pkg: InstalledPackage, libraries: readonly LibraryInfo[]): boolean {
  return libraries.find((l) => l.id === pkg.library_id)?.online === false;
}

/** A stable, well-spread hue per package for the cover placeholder (FNV-1a + avalanche). */
export function placeholderHue(id: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < id.length; i += 1) hash = Math.imul(hash ^ id.charCodeAt(i), 0x01000193);
  hash ^= hash >>> 16;
  hash = Math.imul(hash, 0x45d9f3b);
  hash ^= hash >>> 16;
  return (hash >>> 0) % 360;
}

export function initials(title: string): string {
  const words = title
    .replace(/[^\p{L}\p{N}\s]/gu, " ")
    .split(/\s+/)
    .filter(Boolean);
  const letters = words.slice(0, 2).map((w) => [...w][0] ?? "");
  return letters.join("").toLocaleUpperCase() || "?";
}
