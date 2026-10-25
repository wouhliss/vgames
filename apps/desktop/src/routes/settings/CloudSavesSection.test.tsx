import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it } from "vitest";
import { routes } from "../../app/router";
import { events, type InstalledPackage } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { makeInstalls } from "../../mocks/library";
import { makeSaveHistory } from "../../mocks/saves";
import { flush, renderWithProviders } from "../../test/render";

const [BASE, OTHER_BASE] = makeInstalls(2, MOCK_SERVER, [MOCK_LIBRARY]);

function game(
  base: InstalledPackage | undefined,
  title: string,
  cloud: InstalledPackage["cloud_saves"],
) {
  if (!base) throw new Error("fixture");
  return { ...base, title, state: "installed" as const, running: false, cloud_saves: cloud };
}

const HARBOR = game(BASE, "Hollow Harbor", "synced");
const NO_SAVES = game(OTHER_BASE, "Quiet Quarry", "unsupported");

async function setup(overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: [HARBOR, NO_SAVES],
    saveHistories: { [HARBOR.package.package_id]: makeSaveHistory(HARBOR.package) },
    ...overrides,
  });
  const user = userEvent.setup();
  renderWithProviders(
    <RouterProvider router={createMemoryRouter(routes, { initialEntries: ["/settings/saves"] })} />,
  );
  const section = await screen.findByRole("region", { name: "Cloud saves" });
  return { backend, user, section };
}

async function openHistory(user: ReturnType<typeof userEvent.setup>, section: HTMLElement) {
  await user.click(
    await within(section).findByRole("button", { name: "Save history for Hollow Harbor" }),
  );
  const dialog = await screen.findByRole("dialog", { name: "Save history for Hollow Harbor" });
  await within(dialog).findAllByRole("listitem");
  return dialog;
}

describe("Settings → Cloud saves", () => {
  it("lists only games with cloud saves, with their state", async () => {
    const { section } = await setup();
    expect(await within(section).findByText("Hollow Harbor")).toBeVisible();
    expect(within(section).getByText("Synced")).toBeVisible();
    expect(within(section).queryByText("Quiet Quarry")).toBeNull();
  });

  it("shows the history newest first and only offers Restore for earlier saves", async () => {
    const { user, section } = await setup();
    const dialog = await openHistory(user, section);
    const items = within(dialog).getAllByRole("listitem");
    expect(items).toHaveLength(3);
    expect(items[0]).toHaveTextContent("Cloud save from");
    expect(items[0]).toHaveTextContent("Current");
    expect(within(items[0] as HTMLElement).queryByRole("button")).toBeNull();
    expect(items[1]).toHaveTextContent("Backup on this device from");
    expect(items[1]).toHaveTextContent("2 files");
    expect(items[2]).toHaveTextContent("from Laptop");
  });

  it("restores only after confirming, and Cancel restores nothing", async () => {
    const { backend, user, section } = await setup();
    const dialog = await openHistory(user, section);
    const backup = within(dialog).getAllByRole("listitem")[1] as HTMLElement;
    await user.click(within(backup).getByRole("button", { name: /^Restore the save from/ }));
    const confirm = await screen.findByRole("alertdialog", { name: "Restore this save?" });
    expect(confirm).toHaveTextContent("backed up first");
    await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(backend.callsTo("saves_restore")).toHaveLength(0);

    await user.click(within(backup).getByRole("button", { name: /^Restore the save from/ }));
    await user.click(
      within(await screen.findByRole("alertdialog", { name: "Restore this save?" })).getByRole(
        "button",
        {
          name: "Restore",
        },
      ),
    );
    await waitFor(() =>
      expect(backend.callsTo("saves_restore")[0]?.args).toEqual({
        pkg: HARBOR.package,
        source: { kind: "backup", backup_id: "bacc0000-0000-7000-8000-000000000002" },
      }),
    );
    expect(
      await screen.findByText("Saves for Hollow Harbor restored. Start the game when you want."),
    ).toBeVisible();
    // The game is never started for the player.
    expect(backend.callsTo("game_launch")).toHaveLength(0);
  });

  it.each([
    [{ kind: "network", detail: "offline" }, /couldn't be reached. Nothing changed/],
    [{ kind: "invalid_input", field: "pkg", message: "running" }, /Close the game/],
  ] as const)("explains a refused restore (%o)", async (error, text) => {
    const { user, section } = await setup({ saveRestoreError: error });
    const dialog = await openHistory(user, section);
    const older = within(dialog).getAllByRole("listitem")[2] as HTMLElement;
    await user.click(within(older).getByRole("button", { name: /^Restore the save from/ }));
    await user.click(
      within(await screen.findByRole("alertdialog", { name: "Restore this save?" })).getByRole(
        "button",
        {
          name: "Restore",
        },
      ),
    );
    expect(await screen.findByText(text)).toBeVisible();
  });

  it("follows save-sync-changed", async () => {
    const { backend, section } = await setup();
    await within(section).findByText("Synced");
    backend.state.installs = [{ ...HARBOR, cloud_saves: "pending" }, NO_SAVES];
    await act(async () => {
      await events.saveSyncChanged.emit({
        package: HARBOR.package,
        state: "pending",
        last_synced_at: null,
        conflict_id: null,
      });
      await flush();
    });
    expect(await within(section).findByText("Not synced yet")).toBeVisible();
  });
});
