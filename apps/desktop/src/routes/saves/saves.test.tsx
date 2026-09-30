// Cloud saves (06-cloud-saves §3): every outcome of the sync decision table as the player sees it,
// the conflict dialog (three choices and Cancel, never auto-resolved) and Settings → Cloud saves.
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent, { type UserEvent } from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import { events, type InstalledPackage, type SaveSyncOutcome } from "../../ipc";
import {
  fail,
  installMockBackend,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockState,
} from "../../mocks/backend";
import { makeInstalls } from "../../mocks/library";
import { conflictIdFor, makeConflict } from "../../mocks/saves";
import { flush, renderWithProviders } from "../../test/render";

function game(n: number, title: string, patch: Partial<InstalledPackage>): InstalledPackage {
  const base = makeInstalls(n + 1, MOCK_SERVER, [MOCK_LIBRARY])[n];
  if (!base) throw new Error("fixture");
  return {
    ...base,
    title,
    state: "installed",
    running: false,
    update: null,
    favorite: false,
    collection_ids: [],
    compat: "native",
    targets: [{ id: "play", label: "Play", is_default: true }],
    cloud_saves: "synced",
    ...patch,
  };
}

const SYNCED = game(1, "Quiet Quarry", {});
const CONFLICT = game(2, "Crimson Canyon", { cloud_saves: "conflict" });
const PENDING = game(3, "Paper Planet", { cloud_saves: "pending" });
const PLAIN = game(4, "Plain Plains", { cloud_saves: "unsupported" });
const ALL = [SYNCED, CONFLICT, PENDING, PLAIN];

function start(overrides: Partial<MockState> = {}, path = "/library") {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: ALL,
    actionDelayMs: 0,
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, user: userEvent.setup() };
}

async function sync(title: string, outcome: SaveSyncOutcome, pkg = SYNCED) {
  await act(async () => {
    await events.saveSync.emit({ package: pkg.package, title, outcome });
    await flush();
  });
}

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

const conflictDialog = () =>
  screen.findByRole("dialog", { name: "Which saves do you want to keep?" });

describe("the sync decision table, as the player sees it", () => {
  it("nothing to do: playing starts the game without a word about saves", async () => {
    const { user, backend } = start();
    await user.click(await screen.findByRole("button", { name: "Play Quiet Quarry" }));
    await waitFor(() => expect(backend.callsTo("game_launch")).toHaveLength(1));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByText(/cloud saves/i)).toBeNull();
  });

  it("the cloud is newer: the restore is announced", async () => {
    start();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await sync("Quiet Quarry", { kind: "restored", file_count: 3 });
    expect(
      await screen.findByText("Loaded your newer cloud saves for Quiet Quarry (3 files)."),
    ).toBeVisible();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("this device is newer: the upload after the game is announced", async () => {
    start();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await sync("Quiet Quarry", { kind: "uploaded", file_count: 1 });
    expect(await screen.findByText("Uploaded your saves for Quiet Quarry (1 file).")).toBeVisible();
  });

  it("the server is out of reach: a sync pending notice, and the tile says so", async () => {
    start();
    const tile = await screen.findByRole("article", { name: "Paper Planet" });
    expect(within(tile).getByText("Saves not synced")).toBeVisible();
    await sync("Paper Planet", { kind: "pending" }, PENDING);
    expect(
      await screen.findByText(
        "Paper Planet is using the saves on this device. They sync when the server is back.",
      ),
    ).toBeVisible();
  });

  it("a failed sync says why", async () => {
    start();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await sync("Quiet Quarry", { kind: "failed", detail: "disk full" });
    expect(
      await screen.findByText(/Couldn't sync the saves for Quiet Quarry: disk full/),
    ).toBeVisible();
  });

  it("both sides changed before launch: Play opens the dialog and the game does not start", async () => {
    const { user, backend } = start();
    await user.click(await screen.findByRole("button", { name: "Play Crimson Canyon" }));
    const dialog = await conflictDialog();
    expect(within(dialog).getByText("On this device")).toBeVisible();
    expect(within(dialog).getByText("In the cloud")).toBeVisible();
    expect(within(dialog).getByText("Saved by Sam's desktop")).toBeVisible();
    expect(within(dialog).getByText("Saved by Steam Deck")).toBeVisible();
    expect(within(dialog).getByText("4 files · 3.0 MB")).toBeVisible();
    expect(within(dialog).getByText("5 files · 4.0 MB")).toBeVisible();
    expect(backend.state.installs.find((i) => i.title === "Crimson Canyon")?.running).toBe(false);
  });

  it("both sides changed after the game exited: the dialog opens by itself", async () => {
    start();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await sync(
      "Crimson Canyon",
      { kind: "conflict", conflict_id: conflictIdFor(CONFLICT.package) },
      CONFLICT,
    );
    expect(await conflictDialog()).toBeVisible();
  });
});

describe("the conflict dialog", () => {
  async function open(overrides: Partial<MockState> = {}) {
    const ctx = start(overrides);
    await ctx.user.click(await screen.findByRole("button", { name: "Play Crimson Canyon" }));
    const dialog = await conflictDialog();
    return { ...ctx, dialog };
  }

  it("never decides for the player: nothing is preselected and Continue waits for a choice", async () => {
    const { user, backend, dialog } = await open();
    expect(
      within(dialog)
        .getAllByRole("radio")
        .filter((r) => r.getAttribute("aria-checked") === "true"),
    ).toHaveLength(0);
    const cont = within(dialog).getByRole("button", { name: "Continue" });
    expect(cont).toHaveAttribute("aria-disabled", "true");
    await user.click(cont);
    expect(backend.callsTo("saves_resolve")).toHaveLength(0);
  });

  it.each([
    ["Keep the cloud saves", "keep_cloud"],
    ["Keep the saves on this device", "keep_device"],
    ["Keep both", "keep_both"],
  ])("%s sends exactly that choice", async (label, choice) => {
    const { user, backend, dialog } = await open();
    await user.click(within(dialog).getByRole("radio", { name: new RegExp(`^${label}`) }));
    await user.click(within(dialog).getByRole("button", { name: "Continue" }));
    await waitFor(() => expect(backend.callsTo("saves_resolve")).toHaveLength(1));
    expect(backend.callsTo("saves_resolve")[0]?.args).toEqual({
      conflictId: conflictIdFor(CONFLICT.package),
      choice,
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await screen.findByText("Saves for Crimson Canyon are sorted out.")).toBeVisible();
    // The tile no longer shows the conflict.
    await waitFor(() =>
      expect(
        within(screen.getByRole("article", { name: "Crimson Canyon" })).queryByText(
          "Save conflict",
        ),
      ).toBeNull(),
    );
  });

  it("Cancel and Escape change nothing and send nothing", async () => {
    const { user, backend, dialog } = await open();
    await user.click(within(dialog).getByRole("radio", { name: /^Keep the cloud saves/ }));
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Play Crimson Canyon" }));
    await conflictDialog();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(backend.callsTo("saves_resolve")).toHaveLength(0);
    expect(backend.state.installs.find((i) => i.title === "Crimson Canyon")?.running).toBe(false);
  });

  it("is operable with the keyboard alone", async () => {
    const { user, backend } = await open();
    await user.keyboard("{ArrowDown}{ArrowDown}");
    await user.keyboard(" ");
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("radio", { name: /^Keep both/ })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await user.click(within(dialog).getByRole("button", { name: "Continue" }));
    await waitFor(() => expect(backend.callsTo("saves_resolve")).toHaveLength(1));
  });

  it.each([
    [{ kind: "running" }, /Close Crimson Canyon first/],
    [{ kind: "offline" }, /server can't be reached, so nothing was changed/],
    [{ kind: "not_found" }, /already sorted out/],
    [{ kind: "io", detail: "permission denied" }, /permission denied/],
  ] as const)("a failed choice (%j) says why and keeps the dialog", async (error, text) => {
    const { user, dialog } = await open({
      saves: {
        conflicts: {},
        history: {},
        errors: { saves_resolve: error },
        offline: false,
        resolved: [],
      },
    });
    await user.click(within(dialog).getByRole("radio", { name: /^Keep both/ }));
    await user.click(within(dialog).getByRole("button", { name: "Continue" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(text);
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("when another device pushed again, it shows the newer details and asks again", async () => {
    const newer = {
      ...makeConflict(CONFLICT),
      cloud: {
        changed_at: "2026-09-30T08:00:00Z",
        device_name: "Laptop",
        file_count: 9,
        size_bytes: 1024 * 1024,
      },
    };
    const { user, dialog } = await open({
      saves: {
        conflicts: {},
        history: {},
        errors: { saves_resolve: { kind: "head_moved", conflict: newer } },
        offline: false,
        resolved: [],
      },
    });
    await user.click(within(dialog).getByRole("radio", { name: /^Keep the cloud saves/ }));
    await user.click(within(dialog).getByRole("button", { name: "Continue" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/changed again/);
    expect(within(dialog).getByText("Saved by Laptop")).toBeVisible();
    // The old choice doesn't carry over to saves the player has not seen.
    expect(within(dialog).getByRole("button", { name: "Continue" })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("a conflict that no longer exists says so and offers only Close", async () => {
    const { backend } = start();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    backend.on("saves_conflict", () => fail({ kind: "not_found" }));
    await sync("Crimson Canyon", { kind: "conflict", conflict_id: "gone" }, CONFLICT);
    const dialog = await conflictDialog();
    expect(await within(dialog).findByText("This was already sorted out.")).toBeVisible();
    expect(within(dialog).queryByRole("button", { name: "Continue" })).toBeNull();
    expect(within(dialog).getAllByRole("button", { name: "Close" })).not.toHaveLength(0);
  });
});

describe("Settings → Cloud saves", () => {
  async function openSection(overrides: Partial<MockState> = {}) {
    const ctx = start(overrides, "/settings/cloud-saves");
    const region = await screen.findByRole("region", { name: "Cloud saves" });
    return { ...ctx, region };
  }

  it("lists only games that keep saves in the cloud, with their state", async () => {
    const { region } = await openSection();
    expect(await within(region).findByText("Quiet Quarry")).toBeVisible();
    expect(within(region).getByText("Synced")).toBeVisible();
    expect(within(region).getByText("Waiting to sync")).toBeVisible();
    expect(within(region).getByText("Needs your decision")).toBeVisible();
    expect(within(region).queryByText("Plain Plains")).toBeNull();
  });

  it("says so when no game keeps saves in the cloud", async () => {
    const { region } = await openSection({ installs: [PLAIN] });
    expect(
      await within(region).findByText(/None of your installed games keeps saves in the cloud/),
    ).toBeVisible();
  });

  it("opens the conflict from the list", async () => {
    const { user, region } = await openSection();
    await user.click(
      await within(region).findByRole("button", {
        name: "Resolve the saves conflict of Crimson Canyon",
      }),
    );
    expect(await conflictDialog()).toBeVisible();
  });

  async function openHistory(user: UserEvent, region: HTMLElement) {
    await user.click(
      await within(region).findByRole("button", { name: "Saves history of Quiet Quarry" }),
    );
    return screen.findByRole("dialog", { name: "Saves history of Quiet Quarry" });
  }

  it("shows the server snapshots and the local backups", async () => {
    const { user, region } = await openSection();
    const dialog = await openHistory(user, region);
    expect(within(dialog).getByText("Current")).toBeVisible();
    expect(within(dialog).getAllByText(/Steam Deck/)).not.toHaveLength(0);
    expect(within(dialog).getByText(/Before a restore/)).toBeVisible();
    expect(within(dialog).getByText(/Before a conflict was settled/)).toBeVisible();
    // The current head can't be restored over itself.
    expect(within(dialog).getAllByRole("button", { name: /^Restore the saves from/ })).toHaveLength(
      4,
    );
  });

  it("restores a snapshot after a confirmation that mentions the backup", async () => {
    const { user, backend, region } = await openSection();
    const dialog = await openHistory(user, region);
    const buttons = within(dialog).getAllByRole("button", { name: /^Restore the saves from/ });
    await user.click(buttons[0] as HTMLElement);
    const confirm = await screen.findByRole("alertdialog", { name: "Restore these saves?" });
    expect(within(confirm).getByText(/backed up first/)).toBeVisible();
    expect(backend.callsTo("saves_restore")).toHaveLength(0);
    await user.click(within(confirm).getByRole("button", { name: "Restore" }));
    await waitFor(() => expect(backend.callsTo("saves_restore")).toHaveLength(1));
    expect(backend.callsTo("saves_restore")[0]?.args).toEqual({
      package: SYNCED.package,
      source: { kind: "snapshot", snapshot_id: "snap-2" },
    });
    expect(await screen.findByText("Restored the saves of Quiet Quarry.")).toBeVisible();
  });

  it("restores a local backup", async () => {
    const { user, backend, region } = await openSection();
    const dialog = await openHistory(user, region);
    const buttons = within(dialog).getAllByRole("button", { name: /^Restore the saves from/ });
    await user.click(buttons[buttons.length - 1] as HTMLElement);
    await user.click(await screen.findByRole("button", { name: "Restore" }));
    await waitFor(() => expect(backend.callsTo("saves_restore")).toHaveLength(1));
    expect(backend.callsTo("saves_restore")[0]?.args).toMatchObject({
      source: { kind: "backup", backup_id: "backup-1" },
    });
  });

  it("Cancel in the confirmation restores nothing", async () => {
    const { user, backend, region } = await openSection();
    const dialog = await openHistory(user, region);
    await user.click(
      within(dialog).getAllByRole("button", { name: /^Restore the saves from/ })[0] as HTMLElement,
    );
    await user.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(backend.callsTo("saves_restore")).toHaveLength(0);
  });

  it.each([
    [{ kind: "running" }, /Close Quiet Quarry first/],
    [{ kind: "offline" }, /server can't be reached/],
    [{ kind: "not_found" }, /isn't available anymore/],
    [{ kind: "io", detail: "read-only file system" }, /read-only file system/],
  ] as const)("a failed restore (%j) says why", async (error, text) => {
    const { user, region } = await openSection({
      saves: {
        conflicts: {},
        history: {},
        errors: { saves_restore: error },
        offline: false,
        resolved: [],
      },
    });
    const dialog = await openHistory(user, region);
    await user.click(
      within(dialog).getAllByRole("button", { name: /^Restore the saves from/ })[0] as HTMLElement,
    );
    await user.click(await screen.findByRole("button", { name: "Restore" }));
    const history = await screen.findByRole("dialog", { name: "Saves history of Quiet Quarry" });
    expect(await within(history).findByRole("alert")).toHaveTextContent(text);
  });

  it("offline, the cloud history is unavailable but the backups still are", async () => {
    const { user, region } = await openSection({
      saves: { conflicts: {}, history: {}, errors: {}, offline: true, resolved: [] },
    });
    const dialog = await openHistory(user, region);
    expect(within(dialog).getByText(/No cloud saves yet/)).toBeVisible();
    expect(within(dialog).getByText(/Before a restore/)).toBeVisible();
  });

  it("a history that fails to load offers Try again", async () => {
    const { user, region } = await openSection({
      saves: {
        conflicts: {},
        history: {},
        errors: { saves_history: { kind: "io", detail: "x" } },
        offline: false,
        resolved: [],
      },
    });
    const dialog = await openHistory(user, region);
    expect(await within(dialog).findByText("Couldn't load the history.")).toBeVisible();
    expect(within(dialog).getByRole("button", { name: "Try again" })).toBeVisible();
    cleanup();
  });
});
