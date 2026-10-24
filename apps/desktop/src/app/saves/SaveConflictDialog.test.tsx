import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { events } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { makeInstalls } from "../../mocks/library";
import { makeSaveConflict } from "../../mocks/saves";
import { flush, renderWithProviders } from "../../test/render";
import { routes } from "../router";

const [BASE] = makeInstalls(1, MOCK_SERVER, [MOCK_LIBRARY]);
const GAME = BASE && {
  ...BASE,
  title: "Hollow Harbor",
  state: "installed" as const,
  running: false,
  update: null,
  cloud_saves: "conflict" as const,
  targets: [{ id: "play", label: "Play", is_default: true }],
};

beforeEach(() => {
  // jsdom has no layout: give the library list a size so its tiles render.
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

async function setup(overrides: Partial<MockState> = {}) {
  if (!GAME) throw new Error("fixture");
  const conflict = makeSaveConflict(GAME.package);
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: [GAME],
    actionDelayMs: 0,
    saveConflicts: { [conflict.conflict_id]: conflict },
    launchErrors: {
      [GAME.package.package_id]: { kind: "save_conflict", conflict_id: conflict.conflict_id },
    },
    ...overrides,
  });
  const user = userEvent.setup();
  renderWithProviders(
    <RouterProvider router={createMemoryRouter(routes, { initialEntries: ["/library"] })} />,
  );
  await screen.findByRole("heading", { level: 1, name: "Library" });
  return { backend, user, game: GAME, conflict };
}

async function openFromLaunch(user: ReturnType<typeof userEvent.setup>, title: string) {
  await user.click(await screen.findByRole("button", { name: `Play ${title}` }));
  await user.click(await screen.findByRole("button", { name: "Choose saves" }));
  return screen.findByRole("dialog", { name: `Choose which saves to keep for ${title}` });
}

describe("cloud-save conflict dialog", () => {
  it("opens from the refused launch, shows both sides and keeps the cloud saves", async () => {
    const { backend, user, game, conflict } = await setup();
    const dialog = await openFromLaunch(user, game.title);
    const cloud = await within(dialog).findByRole("region", { name: "In the cloud" });
    expect(cloud).toHaveTextContent("Living room PC");
    expect(cloud).toHaveTextContent("3 files");
    const device = within(dialog).getByRole("region", { name: "On this device" });
    expect(device).toHaveTextContent("This device");
    expect(device).toHaveTextContent("4 files");
    // Nothing is chosen until the player clicks.
    expect(backend.callsTo("save_conflict_resolve")).toHaveLength(0);

    await user.click(within(dialog).getByRole("button", { name: "Keep cloud saves" }));
    await waitFor(() =>
      expect(backend.callsTo("save_conflict_resolve")[0]?.args).toEqual({
        conflictId: conflict.conflict_id,
        choice: "keep_cloud",
      }),
    );
    expect(
      await screen.findByText(
        `Your saves for ${game.title} are ready. Start the game when you want.`,
      ),
    ).toBeVisible();
    expect(screen.queryByRole("dialog")).toBeNull();
    // The game is not relaunched for the player.
    expect(backend.callsTo("game_launch")).toHaveLength(1);
  });

  it("opens on the save-conflict event, and Decide later or Escape change nothing", async () => {
    const { backend, user, conflict } = await setup();
    await act(async () => {
      await events.saveConflict.emit(conflict);
      await flush();
    });
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByRole("region", { name: "In the cloud" });
    await user.click(within(dialog).getByRole("button", { name: "Decide later" }));
    expect(screen.queryByRole("dialog")).toBeNull();

    await act(async () => {
      await events.saveConflict.emit(conflict);
      await flush();
    });
    await within(await screen.findByRole("dialog")).findByRole("region", { name: "In the cloud" });
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(backend.callsTo("save_conflict_resolve")).toHaveLength(0);
  });

  it.each([
    ["keep_device", "Keep this device's saves"],
    ["keep_both", "Keep both"],
  ] as const)("sends %s for its button", async (choice, label) => {
    const { backend, user, game } = await setup();
    const dialog = await openFromLaunch(user, game.title);
    await within(dialog).findByRole("region", { name: "In the cloud" });
    await user.click(within(dialog).getByRole("button", { name: label }));
    await waitFor(() =>
      expect(backend.callsTo("save_conflict_resolve")[0]?.args).toMatchObject({ choice }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("asks again with the new cloud side when it changed meanwhile", async () => {
    const { user, game } = await setup({ saveConflictRenews: true });
    const dialog = await openFromLaunch(user, game.title);
    await within(dialog).findByRole("region", { name: "In the cloud" });
    await user.click(within(dialog).getByRole("button", { name: "Keep cloud saves" }));
    expect(await within(dialog).findByText(/changed again while you were choosing/)).toBeVisible();
    await waitFor(() =>
      expect(within(dialog).getByRole("region", { name: "In the cloud" })).toHaveTextContent(
        "5 files",
      ),
    );
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it.each([
    [{ kind: "network", detail: "offline" }, /couldn't be reached. Nothing changed/],
    [{ kind: "server", code: "save_quota_exceeded", message: "full" }, /storage is full/],
    [{ kind: "invalid_input", field: "conflict_id", message: "running" }, /Close the game first/],
  ] as const)("keeps the conflict and explains %o", async (error, text) => {
    const { user, game } = await setup({ saveResolveError: error });
    const dialog = await openFromLaunch(user, game.title);
    await within(dialog).findByRole("region", { name: "In the cloud" });
    await user.click(within(dialog).getByRole("button", { name: "Keep both" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(text);
    expect(within(dialog).getByRole("button", { name: "Keep both" })).toBeEnabled();
  });
});
