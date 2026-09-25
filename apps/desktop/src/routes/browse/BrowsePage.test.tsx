import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import type { CatalogQuery } from "../../ipc";
import {
  fail,
  installMockBackend,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockState,
} from "../../mocks/backend";
import { catalogHandlers, type MockPackage, makeCatalog } from "../../mocks/catalog";
import { makeInstalls } from "../../mocks/library";
import { flush, renderWithProviders, setRect } from "../../test/render";
import { platformLine } from "./CatalogGrid";

function pkg(i: number, title: string, patch: Partial<MockPackage> = {}): MockPackage {
  const base = makeCatalog(i + 1)[i];
  if (!base) throw new Error("fixture");
  return { ...base, title, slug: title.toLowerCase().replace(/\s+/g, "-"), ...patch };
}

const HARBOR = pkg(0, "Hollow Harbor", {
  genres: ["Adventure"],
  platforms: ["linux-x86_64", "windows-x86_64", "macos-aarch64"],
  updated_at: "2026-09-01T10:00:00Z",
});
const CANYON = pkg(1, "Crimson Canyon", {
  genres: ["Racing"],
  platforms: ["windows-x86_64"],
  updated_at: "2026-09-20T10:00:00Z",
});
const STATION = pkg(2, "Silent Station", {
  genres: ["Adventure", "Puzzle"],
  platforms: ["macos-aarch64"],
  updated_at: "2026-08-01T10:00:00Z",
});
const CATALOG = [HARBOR, CANYON, STATION];

type Backend = ReturnType<typeof installMockBackend>;

/** The real mock handler, for wrapping in a failing one. */
const catalogList = (backend: Backend) => {
  const handler = catalogHandlers(backend.state).catalog_list;
  if (!handler) throw new Error("catalog_list");
  return handler;
};

function setup(overrides: Partial<MockState> = {}, configure?: (backend: Backend) => void) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: [],
    packages: CATALOG,
    actionDelayMs: 0,
    ...overrides,
  });
  configure?.(backend);
  const router = createMemoryRouter(routes, { initialEntries: ["/browse"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

const card = (name: string) => screen.getByRole("link", { name });
const cardNames = () =>
  within(screen.getByTestId("catalog-grid"))
    .getAllByRole("link")
    .map((link) => link.getAttribute("data-package"));
const queries = (backend: Backend) =>
  backend.callsTo("catalog_list").map((c) => c.args.query as CatalogQuery);

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("browse", () => {
  it("lists the catalog with platforms, installed and availability badges", async () => {
    const [installed] = makeInstalls(1, MOCK_SERVER, [MOCK_LIBRARY]);
    if (!installed) throw new Error("fixture");
    setup({
      installs: [{ ...installed, package: { ...installed.package, package_id: HARBOR.id } }],
    });
    await screen.findByRole("link", { name: "Hollow Harbor" });
    expect(cardNames()).toEqual(["crimson-canyon", "hollow-harbor", "silent-station"]);

    const harbor = card("Hollow Harbor");
    expect(harbor).toHaveAttribute("href", `/package/${HARBOR.id}`);
    expect(harbor).toHaveAccessibleDescription("Windows · Mac · Linux Installed");
    expect(card("Crimson Canyon")).toHaveAccessibleDescription("Windows Runs with Proton");
    expect(card("Silent Station")).toHaveAccessibleDescription(
      "Mac Not available on this computer",
    );
    expect(screen.getByText("3 packages")).toHaveAttribute("role", "status");
  });

  it("names each operating system once, whatever the processor", () => {
    expect(platformLine(["macos-aarch64", "macos-x86_64", "linux-aarch64"])).toBe("Mac · Linux");
    expect(platformLine(["windows-aarch64"])).toBe("Windows");
    expect(platformLine([])).toBe("");
  });

  it("opens the details page", async () => {
    const { user, router } = setup();
    await user.click(await screen.findByRole("link", { name: "Crimson Canyon" }));
    await waitFor(() => expect(router.state.location.pathname).toBe(`/package/${CANYON.id}`));
    // Focus follows the navigation to the new page's title, and stays there once it has loaded.
    const title = await screen.findByRole("heading", { level: 1, name: "Crimson Canyon" });
    expect(title).toBeVisible();
    expect(title).toHaveFocus();
  });

  it("searches after the player stops typing, and Escape clears the search", async () => {
    const { user, backend } = setup();
    await screen.findByRole("link", { name: "Hollow Harbor" });
    const search = screen.getByRole("searchbox", { name: "Search the catalog" });
    await user.type(search, "sta");
    await waitFor(() => expect(cardNames()).toEqual(["silent-station"]));
    // Debounced: one request for the whole word, none for each keystroke.
    expect(queries(backend).map((q) => q.query)).toEqual(["", "sta"]);

    await user.keyboard("{Escape}");
    expect(search).toHaveValue("");
    await waitFor(() => expect(cardNames()).toHaveLength(3));
  });

  it("filters by genre with counts, and sorts by recent updates (remembered)", async () => {
    const { user, backend } = setup();
    await screen.findByRole("link", { name: "Hollow Harbor" });
    await user.click(screen.getByRole("combobox", { name: "Genre" }));
    await user.click(screen.getByRole("option", { name: "Adventure (2)" }));
    await waitFor(() => expect(cardNames()).toEqual(["hollow-harbor", "silent-station"]));
    expect(queries(backend).at(-1)).toMatchObject({ genre: "Adventure", cursor: null });

    await user.click(screen.getByRole("combobox", { name: "Sort by" }));
    await user.click(screen.getByRole("option", { name: "Recently updated" }));
    await waitFor(() => expect(cardNames()).toEqual(["hollow-harbor", "silent-station"]));
    expect(queries(backend).at(-1)).toMatchObject({ genre: "Adventure", sort: "recent" });
    expect(localStorage.getItem("vgames.browse.sort")).toBe('"recent"');
  });

  it("offers to clear the filters when nothing matches", async () => {
    const { user } = setup();
    await screen.findByRole("link", { name: "Hollow Harbor" });
    await user.click(screen.getByRole("combobox", { name: "Genre" }));
    await user.click(screen.getByRole("option", { name: "Racing (1)" }));
    await user.type(screen.getByRole("searchbox", { name: "Search the catalog" }), "harbor");
    expect(
      await screen.findByRole("heading", { name: "Nothing matches your search" }),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Clear filters" }));
    await waitFor(() => expect(cardNames()).toHaveLength(3));
    expect(screen.getByRole("searchbox", { name: "Search the catalog" })).toHaveValue("");
    expect(screen.getByRole("combobox", { name: "Genre" })).toHaveTextContent("All genres");
  });

  it("explains an empty server", async () => {
    setup({ packages: [] });
    expect(
      await screen.findByRole("heading", { name: "This server has no packages yet" }),
    ).toBeVisible();
    expect(screen.queryByRole("button", { name: "Clear filters" })).not.toBeInTheDocument();
  });

  it("shows an error with a retry when the catalog cannot be loaded", async () => {
    let failing = true;
    const { user, backend } = setup({}, (backend) => {
      const real = catalogList(backend);
      backend.on("catalog_list", (args) => (failing ? fail({ kind: "offline" }) : real(args)));
    });
    expect(await screen.findByRole("heading", { name: "Couldn't load the catalog" })).toBeVisible();
    failing = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("link", { name: "Hollow Harbor" })).toBeVisible();
    expect(backend.callsTo("catalog_list")).toHaveLength(2);
  });

  it("loads more pages as the player scrolls, without rendering everything", async () => {
    const { backend } = setup({ packages: makeCatalog(120), catalogPageSize: 24 });
    await screen.findAllByRole("link", { name: /./ });
    // The first page fills less than a screen and a half: the next one is fetched right away.
    await waitFor(() => expect(queries(backend).map((q) => q.cursor)).toEqual([null, "24"]));
    const first = cardNames();
    expect(first.length).toBeLessThan(48);

    const main = document.getElementById("main-content");
    if (!main) throw new Error("main");
    Object.defineProperty(main, "scrollTop", { configurable: true, value: 1500 });
    setRect(screen.getByTestId("catalog-grid"), 0, -1500, 1000, 2400);
    await act(async () => {
      fireEvent.scroll(main);
      await flush();
    });
    await waitFor(() => expect(queries(backend).map((q) => q.cursor)).toContain("48"));
    expect(cardNames().some((slug) => !first.includes(slug))).toBe(true);
  });

  it("keeps the loaded packages and offers a retry when the next page fails", async () => {
    let failing = true;
    const { backend, user } = setup(
      { packages: makeCatalog(120), catalogPageSize: 24 },
      (backend) => {
        const real = catalogList(backend);
        backend.on("catalog_list", (args) =>
          failing && (args.query as CatalogQuery).cursor !== null
            ? fail({ kind: "offline" })
            : real(args),
        );
      },
    );
    const retry = await screen.findByRole("button", { name: /Couldn't load more packages/ });
    const loaded = cardNames();
    expect(loaded.length).toBeGreaterThan(0);
    failing = false;
    await user.click(retry);
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /Couldn't load more/ })).not.toBeInTheDocument(),
    );
    expect(queries(backend).filter((q) => q.cursor === "24")).toHaveLength(2);
    expect(cardNames().slice(0, loaded.length)).toEqual(loaded);
  });
});
