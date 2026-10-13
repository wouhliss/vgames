import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it } from "vitest";
import { events } from "../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER } from "../mocks/backend";
import { makeInstalls } from "../mocks/library";
import { flush, renderWithProviders } from "../test/render";
import { routes } from "./router";

const [GAME] = makeInstalls(1, MOCK_SERVER, [MOCK_LIBRARY]);

async function setup() {
  if (!GAME) throw new Error("fixture");
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: [GAME],
    packageOverlays: [
      {
        package: GAME.package,
        title: GAME.title,
        enabled: false,
        disabled_by_safety_valve_at: "2026-10-12T10:00:00Z",
      },
    ],
  });
  const user = userEvent.setup();
  renderWithProviders(
    <RouterProvider router={createMemoryRouter(routes, { initialEntries: ["/library"] })} />,
  );
  await screen.findByRole("heading", { level: 1, name: "Library" });
  return { backend, user, game: GAME };
}

async function tripped(pkg: { server_id: string; package_id: string }) {
  await act(async () => {
    await events.overlayPackageDisabled.emit({ package: pkg });
    await flush();
  });
}

describe("overlay turned off by the safety valve", () => {
  it("names the game in plain text and turns the overlay back on", async () => {
    const { backend, user, game } = await setup();
    expect(screen.queryByRole("region", { name: "In-game overlay turned off" })).toBeNull();
    await tripped(game.package);
    const notice = await screen.findByRole("region", { name: "In-game overlay turned off" });
    expect(notice).toHaveTextContent(`The in-game overlay is off for ${game.title}.`);
    expect(notice.querySelector("a, [href]")).toBeNull();
    await user.click(within(notice).getByRole("button", { name: "Turn it back on" }));
    await waitFor(() =>
      expect(backend.callsTo("package_overlay_set")[0]?.args).toEqual({
        packageRef: game.package,
        enabled: true,
      }),
    );
    expect(await screen.findByText("The overlay is back on for this game")).toBeVisible();
    expect(screen.queryByRole("region", { name: "In-game overlay turned off" })).toBeNull();
  });

  it("can be dismissed without changing the setting, and keeps working by keyboard", async () => {
    const { backend, user, game } = await setup();
    await tripped(game.package);
    await tripped(game.package);
    const notices = await screen.findAllByRole("region", { name: "In-game overlay turned off" });
    expect(notices).toHaveLength(1);
    within(notices[0] as HTMLElement)
      .getByRole("button", { name: "Dismiss" })
      .focus();
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("region", { name: "In-game overlay turned off" })).toBeNull();
    expect(backend.callsTo("package_overlay_set")).toHaveLength(0);
  });

  it("still explains when the game is not in the library", async () => {
    await setup();
    await tripped({
      server_id: MOCK_SERVER.id,
      package_id: "01920000-0000-7000-8000-00000000beef",
    });
    expect(
      await screen.findByRole("region", { name: "In-game overlay turned off" }),
    ).toHaveTextContent("The in-game overlay is off for a game.");
  });
});
