import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import { events, type InstalledPackage, type LibraryInfo } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { MOCK_COLLECTIONS, makeInstalls, OFFLINE_LIBRARY } from "../../mocks/library";
import { flush, renderWithProviders, setRect } from "../../test/render";

const [COOP, BACKLOG, FINISHED] = MOCK_COLLECTIONS.map((c) => c.id) as [string, string, string];

const SSD: LibraryInfo = {
  ...MOCK_LIBRARY,
  id: "01920000-0000-7000-8000-0000000000b3",
  path: "/mnt/ssd",
  label: "Fast SSD",
  is_default: false,
  free_bytes: 900 * 1024 ** 3,
};

function fixture(i: number, title: string, patch: Partial<InstalledPackage> = {}) {
  const base = makeInstalls(1, MOCK_SERVER, [MOCK_LIBRARY])[0];
  if (!base) throw new Error("fixture");
  return {
    ...base,
    title,
    slug: title.toLowerCase().replace(/\s+/g, "-"),
    package: {
      ...base.package,
      package_id: `0192a6f0-1c2d-7e3f-8a9b-${String(i).padStart(12, "0")}`,
    },
    favorite: false,
    collection_ids: [],
    update: null,
    running: false,
    state: "installed" as const,
    compat: "native" as const,
    cloud_saves: "unsupported" as const,
    targets: [{ id: "play", label: "Play", is_default: true }],
    last_played_at: `2026-09-${String(20 - i).padStart(2, "0")}T10:00:00Z`,
    ...patch,
  } satisfies InstalledPackage;
}

const HOLLOW = fixture(1, "Hollow Harbor", {
  favorite: true,
  collection_ids: [COOP],
  targets: [
    { id: "play", label: "Play", is_default: true },
    { id: "editor", label: "Level editor", is_default: false },
  ],
});
const CRIMSON = fixture(2, "Crimson Canyon", {
  size_bytes: 30 * 1024 ** 3,
  update: { version_label: "2.0.0", sequence: 9, download_bytes: 1e9, installed_yanked: false },
});
const SILENT = fixture(3, "Silent Station", { state: "incomplete", targets: [], compat: "proton" });
const GILDED = fixture(4, "Gilded Garden", { library_id: OFFLINE_LIBRARY.id });
const FROZEN = fixture(5, "Frozen Forge", {
  update: { version_label: "1.9.0", sequence: 4, download_bytes: 1e9, installed_yanked: true },
});
const ELECTRIC = fixture(6, "Electric Engine", { running: true, cloud_saves: "conflict" });
const ALL = [HOLLOW, CRIMSON, SILENT, GILDED, FROZEN, ELECTRIC];

function setup(overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY, OFFLINE_LIBRARY],
    collections: MOCK_COLLECTIONS,
    installs: ALL,
    actionDelayMs: 0,
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: ["/library"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

const tile = (name: string) => screen.getByRole("article", { name });

async function openMenu(user: ReturnType<typeof userEvent.setup>, title: string) {
  await user.click(within(tile(title)).getByRole("button", { name: `More actions for ${title}` }));
  return screen.getByRole("menu", { name: `Actions for ${title}` });
}

/** A DataTransfer stand-in (jsdom has none). */
function dataTransfer() {
  const store = new Map<string, string>();
  return {
    setData: (type: string, value: string) => store.set(type, value),
    getData: (type: string) => store.get(type) ?? "",
    get types() {
      return [...store.keys()];
    },
    effectAllowed: "all",
    dropEffect: "none",
  };
}

beforeEach(() => {
  localStorage.clear();
  // jsdom has no layout: give the scroll container and the list a size.
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("library", () => {
  it("shows the empty library with a way to the catalog", async () => {
    const { user, router } = setup({ installs: [] });
    expect(await screen.findByRole("heading", { name: "Your library is empty" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Browse the catalog" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/browse"));
  });

  it("shows an error with a retry when the library cannot be read", async () => {
    const backend = installMockBackend({
      servers: [MOCK_SERVER],
      libraries: [MOCK_LIBRARY],
      installs: ALL,
      collections: MOCK_COLLECTIONS,
    });
    let fail = true;
    const list = () => (fail ? Promise.reject({ kind: "internal", detail: "boom" }) : ALL);
    backend.on("installs_list", list);
    const router = createMemoryRouter(routes, { initialEntries: ["/library"] });
    renderWithProviders(<RouterProvider router={router} />);
    const user = userEvent.setup();
    expect(
      await screen.findByRole("heading", { name: "Couldn't load your library" }),
    ).toBeVisible();
    fail = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("article", { name: "Hollow Harbor" })).toBeVisible();
  });

  it("keeps the shown list when a refresh fails", async () => {
    const { backend } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    backend.on("installs_list", () => Promise.reject({ kind: "internal", detail: "boom" }));
    await act(async () => {
      await events.installsChanged.emit({});
      await flush();
    });
    expect(screen.getByRole("article", { name: "Hollow Harbor" })).toBeInTheDocument();
  });

  it("pins favorites above everything else and shows each state", async () => {
    setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    const headings = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
    expect(headings).toEqual(["Favorites 1 package", "Everything else 5 packages"]);
    const articles = screen.getAllByRole("article").map((a) => a.getAttribute("data-package"));
    expect(articles[0]).toBe("hollow-harbor");
    expect(tile("Crimson Canyon")).toHaveTextContent("Update available");
    expect(tile("Frozen Forge")).toHaveTextContent("Update recommended");
    expect(tile("Silent Station")).toHaveTextContent("Incomplete");
    expect(tile("Silent Station")).toHaveTextContent("Proton");
    expect(tile("Gilded Garden")).toHaveTextContent("Library offline");
    expect(tile("Electric Engine")).toHaveTextContent("Running");
    expect(tile("Electric Engine")).toHaveTextContent("Save conflict");
    // Badges describe the tile's main button for screen readers.
    expect(
      within(tile("Silent Station")).getByRole("button", { name: "Silent Station" }),
    ).toHaveAccessibleDescription(/Incomplete/);
    // Favorites say so in the name of their main button.
    expect(
      within(tile("Hollow Harbor")).getByRole("button", { name: "Favorite Hollow Harbor" }),
    ).toBeInTheDocument();
  });

  it("searches, reports no match and clears the search", async () => {
    const { user } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    const search = screen.getByRole("searchbox", { name: "Search your library" });
    await user.type(search, "canyon");
    await waitFor(() => expect(screen.getAllByRole("article")).toHaveLength(1));
    expect(tile("Crimson Canyon")).toBeInTheDocument();
    await user.clear(search);
    await user.type(search, "zzz");
    expect(await screen.findByRole("heading", { name: "Nothing matches “zzz”" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Clear search" }));
    await waitFor(() => expect(screen.getAllByRole("article")).toHaveLength(6));
  });

  it("sorts by name in the list layout and remembers the choice", async () => {
    const { user } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    await user.click(screen.getByRole("button", { name: "List" }));
    expect(screen.getByRole("button", { name: "List" })).toHaveAttribute("aria-pressed", "true");
    await user.click(screen.getByRole("combobox", { name: "Sort by" }));
    await user.click(screen.getByRole("option", { name: "Name" }));
    const order = screen.getAllByRole("article").map((a) => a.getAttribute("data-package"));
    expect(order).toEqual([
      "hollow-harbor",
      "crimson-canyon",
      "electric-engine",
      "frozen-forge",
      "gilded-garden",
      "silent-station",
    ]);
    expect(JSON.parse(localStorage.getItem("vgames.library.sort") ?? "null")).toBe("name");
    expect(JSON.parse(localStorage.getItem("vgames.library.view") ?? "null")).toBe("list");
  });

  it("Play launches the default target, and Stop asks first", async () => {
    const { user, backend } = setup();
    await user.click(await screen.findByRole("button", { name: "Play Hollow Harbor" }));
    expect(backend.callsTo("game_launch")[0]?.args).toEqual({
      pkg: HOLLOW.package,
      targetId: null,
    });
    const stop = await screen.findByRole("button", { name: "Stop Hollow Harbor" });
    await user.click(stop);
    const dialog = screen.getByRole("alertdialog", { name: "Stop Hollow Harbor?" });
    // The safe choice has focus.
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.click(within(dialog).getByRole("button", { name: "Stop game" }));
    expect(backend.callsTo("game_stop")[0]?.args).toEqual({ pkg: HOLLOW.package });
    expect(await screen.findByRole("button", { name: "Play Hollow Harbor" })).toBeVisible();
  });

  it("offers the manifest's other launch targets in the menu", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    const menu = await openMenu(user, "Hollow Harbor");
    await user.click(within(menu).getByRole("menuitem", { name: "Launch: Level editor" }));
    expect(backend.callsTo("game_launch")[0]?.args).toEqual({
      pkg: HOLLOW.package,
      targetId: "editor",
    });
  });

  it("a failed file check explains it and offers Verify files", async () => {
    const { user, backend } = setup({
      launchErrors: { [CRIMSON.package.package_id]: { kind: "integrity", path: "Game/game.exe" } },
    });
    await user.click(await screen.findByRole("button", { name: "Play Crimson Canyon" }));
    const alert = await screen.findByText(
      /doesn't match what the server published \(Game\/game\.exe\)/,
    );
    expect(alert).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Verify files" }));
    expect(backend.callsTo("install_verify")[0]?.args).toEqual({ pkg: CRIMSON.package });
  });

  it("every launch error has its own message", async () => {
    const cases = [
      [{ kind: "key_revoked" }, /replaced the signature/],
      [{ kind: "compat_unavailable", detail: "Vulkan driver missing" }, /Vulkan driver missing/],
      [{ kind: "rate_limited" }, /Wait a moment/],
      [{ kind: "save_conflict", conflict_id: "c1" }, /changed on this device and in the cloud/],
      [{ kind: "already_running" }, /already running/],
      [{ kind: "io", detail: "permission denied" }, /permission denied/],
    ] as const;
    const { user, backend } = setup();
    for (const [error, text] of cases) {
      backend.state.launchErrors = { [CRIMSON.package.package_id]: error };
      await user.click(await screen.findByRole("button", { name: "Play Crimson Canyon" }));
      expect(await screen.findByText(text)).toBeVisible();
    }
  });

  it("an offline library keeps Play unavailable", async () => {
    const { user, backend } = setup();
    const play = await screen.findByRole("button", { name: "Play Gilded Garden" });
    expect(play).toHaveAttribute("aria-disabled", "true");
    await user.click(play);
    expect(backend.callsTo("game_launch")).toHaveLength(0);
  });

  it("an incomplete install can be removed or resumed", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Silent Station" });
    const menu = await openMenu(user, "Silent Station");
    expect(within(menu).getByRole("menuitem", { name: "Remove…" })).toBeInTheDocument();
    expect(within(menu).queryByRole("menuitem", { name: "Verify files" })).not.toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Resume installing Silent Station" }));
    expect(backend.callsTo("install_resume")[0]?.args).toEqual({ pkg: SILENT.package });
    expect(await screen.findByText("Silent Station will continue downloading")).toBeVisible();
    expect(
      await within(tile("Silent Station")).findByRole("button", { name: "View download" }),
    ).toBeVisible();
  });

  it("favorites toggle from the menu and move to the top", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Crimson Canyon" });
    const menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Add to favorites" }));
    expect(backend.callsTo("favorite_set")[0]?.args).toEqual({
      pkg: CRIMSON.package,
      favorite: true,
    });
    await waitFor(() =>
      expect(screen.getAllByRole("heading", { level: 2 })[0]).toHaveTextContent(
        "Favorites 2 packages",
      ),
    );
  });

  it("adds to collections from the menu, creating one on the way", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Crimson Canyon" });
    const menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Add to collection…" }));
    const dialog = screen.getByRole("dialog", { name: "Add Crimson Canyon to collections" });
    await user.click(within(dialog).getByRole("checkbox", { name: "Backlog" }));
    // Focus stays on the checkbox while the change is saved (keyboard and controller users).
    expect(within(dialog).getByRole("checkbox", { name: "Backlog" })).toHaveFocus();
    expect(backend.callsTo("collection_add_package")[0]?.args).toEqual({
      collectionId: BACKLOG,
      pkg: CRIMSON.package,
    });
    await waitFor(() =>
      expect(within(dialog).getByRole("checkbox", { name: "Backlog" })).toBeChecked(),
    );
    await user.click(within(dialog).getByRole("checkbox", { name: "Backlog" }));
    expect(backend.callsTo("collection_remove_package")).toHaveLength(1);

    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(within(dialog).getByText("Enter a name with 1 to 100 characters.")).toBeVisible();
    await user.type(within(dialog).getByRole("textbox", { name: "New collection" }), "backlog");
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(
      await within(dialog).findByText("You already have a collection with this name."),
    ).toBeVisible();
    const field = within(dialog).getByRole("textbox", { name: "New collection" });
    await user.clear(field);
    await user.type(field, "Weekend{Enter}");
    await waitFor(() => expect(backend.callsTo("collection_create")).toHaveLength(2));
    await waitFor(() =>
      expect(within(dialog).getByRole("checkbox", { name: "Weekend" })).toBeChecked(),
    );
    await user.click(within(dialog).getByRole("button", { name: "Done" }));
    expect(screen.getByRole("tab", { name: "Weekend 1" })).toBeInTheDocument();
  });

  it("manages collections: rename, reorder and delete", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    await user.click(screen.getByRole("button", { name: "Collections…" }));
    const dialog = screen.getByRole("dialog", { name: "Collections" });

    await user.click(within(dialog).getByRole("button", { name: "Rename Backlog" }));
    const field = within(dialog).getByRole("textbox", { name: "New name for Backlog" });
    expect(field).toHaveFocus();
    await user.clear(field);
    await user.type(field, "Later{Enter}");
    expect(backend.callsTo("collection_rename")[0]?.args).toEqual({
      collectionId: BACKLOG,
      name: "Later",
    });
    await waitFor(() => expect(within(dialog).getByText("Later")).toBeVisible());

    const up = within(dialog).getByRole("button", { name: "Move Co-op nights up" });
    expect(up).toHaveAttribute("aria-disabled", "true");
    await user.click(within(dialog).getByRole("button", { name: "Move Finished up" }));
    expect(backend.callsTo("collections_reorder")[0]?.args).toEqual({
      collectionIds: [COOP, FINISHED, BACKLOG],
    });

    await user.click(within(dialog).getByRole("button", { name: "Delete Co-op nights" }));
    const confirm = screen.getByRole("alertdialog", {
      name: "Delete the collection “Co-op nights”?",
    });
    await user.click(within(confirm).getByRole("button", { name: "Delete collection" }));
    expect(backend.callsTo("collection_delete")[0]?.args).toEqual({ collectionId: COOP });
    await waitFor(() =>
      expect(screen.queryByRole("tab", { name: /Co-op nights/ })).not.toBeInTheDocument(),
    );
  });

  it("reorders collections by drag and drop", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    await user.click(screen.getByRole("button", { name: "Collections…" }));
    const dialog = screen.getByRole("dialog", { name: "Collections" });
    const rows = within(dialog).getAllByRole("listitem");
    const transfer = dataTransfer();
    fireEvent.dragStart(rows[2] as HTMLElement, { dataTransfer: transfer });
    fireEvent.dragOver(rows[0] as HTMLElement, { dataTransfer: transfer });
    fireEvent.drop(rows[0] as HTMLElement, { dataTransfer: transfer });
    await waitFor(() =>
      expect(backend.callsTo("collections_reorder")[0]?.args).toEqual({
        collectionIds: [FINISHED, COOP, BACKLOG],
      }),
    );
  });

  it("dragging a package onto a collection tab adds it", async () => {
    const { backend } = setup();
    await screen.findByRole("article", { name: "Crimson Canyon" });
    const transfer = dataTransfer();
    fireEvent.dragStart(
      within(tile("Crimson Canyon")).getByRole("button", { name: "Crimson Canyon" }),
      {
        dataTransfer: transfer,
      },
    );
    const tab = screen.getByRole("tab", { name: /Finished/ });
    fireEvent.dragOver(tab, { dataTransfer: transfer });
    expect(tab).toHaveAttribute("data-drop-active", "true");
    fireEvent.drop(tab, { dataTransfer: transfer });
    await waitFor(() =>
      expect(backend.callsTo("collection_add_package")[0]?.args).toEqual({
        collectionId: FINISHED,
        pkg: CRIMSON.package,
      }),
    );
    expect(await screen.findByText("Added Crimson Canyon to Finished")).toBeVisible();
  });

  it("collection tabs filter, and LB/RB switch them", async () => {
    const { user } = setup();
    await screen.findByRole("article", { name: "Hollow Harbor" });
    await user.click(screen.getByRole("tab", { name: "Co-op nights 1" }));
    expect(screen.getAllByRole("article").map((a) => a.getAttribute("data-package"))).toEqual([
      "hollow-harbor",
    ]);
    await act(async () => {
      await events.uiNav.emit({ action: "tab_next", controller: "xinput", repeat: false });
      await flush();
    });
    expect(screen.getByRole("tab", { name: "Backlog 0" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("heading", { name: "This collection is empty" })).toBeVisible();
    await user.click(screen.getByRole("tab", { name: "Favorites 1" }));
    expect(screen.getAllByRole("article")).toHaveLength(1);
  });

  it("uninstalling asks about leftover files and keeps them unless ticked", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Crimson Canyon" });
    let menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Uninstall…" }));
    let dialog = screen.getByRole("alertdialog", { name: "Uninstall Crimson Canyon?" });
    expect(await within(dialog).findByText("Files you added")).toBeVisible();
    expect(within(dialog).getByText("Saved/profile.sav")).toBeVisible();
    expect(
      within(dialog).getByRole("checkbox", { name: /Also delete these files/ }),
    ).not.toBeChecked();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(backend.callsTo("install_uninstall")).toHaveLength(0);

    menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Uninstall…" }));
    dialog = screen.getByRole("alertdialog", { name: "Uninstall Crimson Canyon?" });
    await user.click(
      await within(dialog).findByRole("checkbox", { name: /Also delete these files/ }),
    );
    await user.click(within(dialog).getByRole("button", { name: "Uninstall" }));
    expect(backend.callsTo("install_uninstall")[0]?.args).toEqual({
      package: CRIMSON.package,
      removeLeftovers: true,
      removePrefix: false,
    });
    await waitFor(() =>
      expect(screen.queryByRole("article", { name: "Crimson Canyon" })).not.toBeInTheDocument(),
    );
  });

  it("uninstalling a Proton package asks about its compatibility files", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Silent Station" });
    const menu = await openMenu(user, "Silent Station");
    await user.click(within(menu).getByRole("menuitem", { name: "Remove…" }));
    const dialog = screen.getByRole("alertdialog", { name: "Uninstall Silent Station?" });
    const prefix = await within(dialog).findByRole("checkbox", {
      name: /Windows compatibility files/,
    });
    await user.click(prefix);
    await user.click(within(dialog).getByRole("button", { name: "Uninstall" }));
    expect(backend.callsTo("install_uninstall")[0]?.args).toEqual({
      package: SILENT.package,
      removeLeftovers: false,
      removePrefix: true,
    });
  });

  it("moves to another library, never to an offline one", async () => {
    const { user, backend } = setup({ libraries: [MOCK_LIBRARY, OFFLINE_LIBRARY, SSD] });
    await screen.findByRole("article", { name: "Crimson Canyon" });
    const menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Move to another library…" }));
    const dialog = screen.getByRole("dialog", { name: "Move Crimson Canyon" });
    expect(within(dialog).getByRole("radio", { name: "External drive" })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    expect(within(dialog).getByRole("radio", { name: "Fast SSD" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await user.click(within(dialog).getByRole("button", { name: "Move" }));
    expect(backend.callsTo("install_move")[0]?.args).toEqual({
      package: CRIMSON.package,
      libraryId: SSD.id,
    });
  });

  it("shows why a move failed", async () => {
    const { user } = setup({
      libraries: [MOCK_LIBRARY, SSD],
      actionErrors: {
        [`install_move:${CRIMSON.package.package_id}`]: {
          kind: "insufficient_space",
          required_bytes: 30 * 1024 ** 3,
          available_bytes: 10 * 1024 ** 3,
        },
      },
    });
    await screen.findByRole("article", { name: "Crimson Canyon" });
    const menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Move to another library…" }));
    const dialog = screen.getByRole("dialog", { name: "Move Crimson Canyon" });
    await user.click(within(dialog).getByRole("button", { name: "Move" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Not enough space: 30.0 GB needed, 10.0 GB free.",
    );
  });

  it("an update over a withdrawn version asks first", async () => {
    const { user, backend } = setup();
    await screen.findByRole("article", { name: "Frozen Forge" });
    const menu = await openMenu(user, "Frozen Forge");
    await user.click(within(menu).getByRole("menuitem", { name: "Update to 1.9.0" }));
    const dialog = screen.getByRole("alertdialog", {
      name: "Install version 1.9.0 of Frozen Forge?",
    });
    expect(backend.callsTo("install_update")).toHaveLength(0);
    await user.click(within(dialog).getByRole("button", { name: "Install 1.9.0" }));
    expect(backend.callsTo("install_update")[0]?.args).toEqual({ pkg: FROZEN.package });
    expect(await screen.findByText("The update for Frozen Forge is queued")).toBeVisible();
  });

  it("creates shortcuts and reports action errors", async () => {
    const { user } = setup({
      actionErrors: {
        [`install_verify:${CRIMSON.package.package_id}`]: { kind: "offline" },
      },
    });
    await screen.findByRole("article", { name: "Crimson Canyon" });
    let menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Create desktop shortcut" }));
    expect(await screen.findByText("Desktop shortcut created")).toBeVisible();
    menu = await openMenu(user, "Crimson Canyon");
    await user.click(within(menu).getByRole("menuitem", { name: "Verify files" }));
    expect(await screen.findByText(/The server can't be reached/)).toBeVisible();
  });

  it("Shift+F10 opens the same actions from the keyboard", async () => {
    const { user } = setup();
    const main = within(await screen.findByRole("article", { name: "Crimson Canyon" })).getByRole(
      "button",
      { name: "Crimson Canyon" },
    );
    main.focus();
    await user.keyboard("{Shift>}{F10}{/Shift}");
    const menu = screen.getByRole("menu", { name: "Actions for Crimson Canyon" });
    expect(within(menu).getByRole("menuitem", { name: "Details" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(main).toHaveFocus();
  });

  it("renders only the visible part of 5,000 packages", async () => {
    setup({
      installs: makeInstalls(5000, MOCK_SERVER, [MOCK_LIBRARY, OFFLINE_LIBRARY]),
    });
    await screen.findAllByRole("article");
    const first = screen.getAllByRole("article").map((a) => a.getAttribute("data-package"));
    expect(first.length).toBeGreaterThan(6);
    expect(first.length).toBeLessThan(120);
    expect(screen.getByRole("tab", { name: "All 5000" })).toBeInTheDocument();
    const main = document.getElementById("main-content");
    if (!main) throw new Error("main");
    // jsdom has no scrolling: report a scrolled position the way a browser would (the list's box
    // moves up by the same amount).
    Object.defineProperty(main, "scrollTop", { configurable: true, value: 200_000 });
    setRect(screen.getByTestId("library-view"), 0, -200_000, 1000, 300_000);
    await act(async () => {
      fireEvent.scroll(main);
      await flush();
    });
    const later = screen.getAllByRole("article").map((a) => a.getAttribute("data-package"));
    expect(later.length).toBeLessThan(120);
    expect(later.some((slug) => first.includes(slug))).toBe(false);
  });
});
