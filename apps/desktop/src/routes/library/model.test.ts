import { describe, expect, it } from "vitest";
import type { InstalledPackage } from "../../ipc";
import { MOCK_LIBRARY, MOCK_SERVER } from "../../mocks/backend";
import { makeInstalls, OFFLINE_LIBRARY } from "../../mocks/library";
import {
  buildRows,
  columnsFor,
  filterInstalls,
  initials,
  isOffline,
  placeholderHue,
  rowHeight,
  searchKey,
  sectionsOf,
  sortInstalls,
} from "./model";

function pkg(title: string, patch: Partial<InstalledPackage> = {}): InstalledPackage {
  const [base] = makeInstalls(1, MOCK_SERVER, [MOCK_LIBRARY]);
  if (!base) throw new Error("fixture");
  return {
    ...base,
    title,
    package: { ...base.package, package_id: `id-${title}` },
    favorite: false,
    collection_ids: [],
    ...patch,
  };
}

describe("library model", () => {
  it("searches without case or diacritics", () => {
    const items = [pkg("Ōkami Journey"), pkg("Portal Runner"), pkg("Überfall")];
    expect(filterInstalls(items, "okami", "all").map((p) => p.title)).toEqual(["Ōkami Journey"]);
    expect(filterInstalls(items, "  UBER ", "all").map((p) => p.title)).toEqual(["Überfall"]);
    expect(filterInstalls(items, "", "all")).toHaveLength(3);
    expect(searchKey("Ǆ Café")).toBe("dz cafe");
  });

  it("filters by favorites and collection", () => {
    const items = [
      pkg("A", { favorite: true }),
      pkg("B", { collection_ids: ["c1"] }),
      pkg("C", { collection_ids: ["c1", "c2"], favorite: true }),
    ];
    expect(filterInstalls(items, "", "favorites").map((p) => p.title)).toEqual(["A", "C"]);
    expect(filterInstalls(items, "", "c:c1").map((p) => p.title)).toEqual(["B", "C"]);
    expect(filterInstalls(items, "", "c:c2").map((p) => p.title)).toEqual(["C"]);
    expect(filterInstalls(items, "b", "c:c1").map((p) => p.title)).toEqual(["B"]);
  });

  it("sorts by every key, with stable tie-breaks by name", () => {
    const items = [
      pkg("beta", {
        size_bytes: 10,
        last_played_at: null,
        installed_at: "2026-01-02T00:00:00Z",
      }),
      pkg("Alpha 10", {
        size_bytes: 30,
        last_played_at: "2026-03-01T00:00:00Z",
        installed_at: "2026-01-01T00:00:00Z",
      }),
      pkg("Alpha 9", {
        size_bytes: 30,
        last_played_at: "2026-04-01T00:00:00Z",
        installed_at: null,
      }),
    ];
    const titles = (key: Parameters<typeof sortInstalls>[1]) =>
      sortInstalls(items, key).map((p) => p.title);
    expect(titles("name")).toEqual(["Alpha 9", "Alpha 10", "beta"]);
    expect(titles("recent")).toEqual(["Alpha 9", "Alpha 10", "beta"]);
    expect(titles("size")).toEqual(["Alpha 9", "Alpha 10", "beta"]);
    expect(titles("installed")).toEqual(["beta", "Alpha 10", "Alpha 9"]);
  });

  it("pins favorites in their own section at the top", () => {
    const items = [pkg("A"), pkg("B", { favorite: true }), pkg("C")];
    const sections = sectionsOf(items, "all");
    expect(sections.map((s) => [s.id, s.items.map((p) => p.title)])).toEqual([
      ["favorites", ["B"]],
      ["others", ["A", "C"]],
    ]);
    expect(sectionsOf([pkg("A")], "all").map((s) => s.id)).toEqual(["others"]);
    expect(sectionsOf([pkg("A", { favorite: true })], "favorites").map((s) => s.id)).toEqual([
      "favorites",
    ]);
  });

  it("builds rows of N tiles, with headers only for two sections", () => {
    const favorites = Array.from({ length: 3 }, (_, i) => pkg(`F${i}`, { favorite: true }));
    const others = Array.from({ length: 7 }, (_, i) => pkg(`O${i}`));
    const rows = buildRows(sectionsOf([...favorites, ...others], "all"), 4);
    expect(rows.map((r) => (r.kind === "header" ? `h:${r.count}` : r.items.length))).toEqual([
      "h:3",
      3,
      "h:7",
      4,
      3,
    ]);
    const single = buildRows(sectionsOf(others, "all"), 3);
    expect(single.every((r) => r.kind === "items")).toBe(true);
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length);
  });

  it("computes columns and exact row heights from the width", () => {
    expect(columnsFor(0)).toBe(1);
    expect(columnsFor(150)).toBe(1);
    expect(columnsFor(316)).toBe(2);
    expect(columnsFor(1000)).toBe(6);
    const [row] = buildRows(sectionsOf([pkg("A")], "all"), 6);
    if (!row) throw new Error("row");
    expect(rowHeight(row, "list", 1000, 1)).toBe(64);
    // (1000 − 5·16) / 6 = 153.3 px wide → 230 px cover + 96 footer + 16 gap.
    expect(rowHeight(row, "grid", 1000, 6)).toBe(342);
  });

  it("knows when a package's drive is offline", () => {
    const libraries = [MOCK_LIBRARY, OFFLINE_LIBRARY];
    expect(isOffline(pkg("A", { library_id: OFFLINE_LIBRARY.id }), libraries)).toBe(true);
    expect(isOffline(pkg("A", { library_id: MOCK_LIBRARY.id }), libraries)).toBe(false);
    expect(isOffline(pkg("A", { library_id: "unknown" }), libraries)).toBe(false);
  });

  it("derives placeholder initials and well-spread hues", () => {
    expect(initials("Hollow Harbor")).toBe("HH");
    expect(initials("🚀 Rocket Rally")).toBe("RR");
    expect(initials("مغامرة الصحراء")).toBe("ما");
    expect(initials("!!!")).toBe("?");
    const hues = new Set(
      makeInstalls(60, MOCK_SERVER, [MOCK_LIBRARY]).map((p) =>
        Math.floor(placeholderHue(p.package.package_id) / 30),
      ),
    );
    expect(hues.size).toBeGreaterThanOrEqual(10);
  });

  it("generates 5,000 distinct fixtures", () => {
    const items = makeInstalls(5000, MOCK_SERVER, [MOCK_LIBRARY, OFFLINE_LIBRARY]);
    expect(new Set(items.map((p) => p.title)).size).toBe(5000);
    expect(new Set(items.map((p) => p.package.package_id)).size).toBe(5000);
  });
});
