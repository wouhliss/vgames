import { act, cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it } from "vitest";
import { routes } from "../../app/router";
import { commands, events } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER } from "../../mocks/backend";
import { makeInstalls } from "../../mocks/library";
import { flush, renderWithProviders } from "../../test/render";

const games = makeInstalls(2, MOCK_SERVER, [MOCK_LIBRARY]);
const game = (() => {
  const first = games.at(0);
  if (!first) throw new Error("Game fixture missing");
  return first;
})();
const KEY = "controller-profile-ui-restart";
function start() {
  const backend = installMockBackend({
    persistKey: KEY,
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: games,
  });
  renderWithProviders(
    <RouterProvider
      router={createMemoryRouter(routes, { initialEntries: ["/settings/controllers"] })}
    />,
  );
  return { backend, user: userEvent.setup() };
}
async function chooseGame(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("combobox", { name: "Game" }));
  await user.click(
    await screen.findByRole("option", { name: `${game.title} · ${MOCK_SERVER.name}` }),
  );
  return screen.findByRole("button", { name: "Save controller profile" });
}
beforeEach(() => localStorage.clear());

describe("controller package profiles", () => {
  it("keeps mode, stick settings and remaps after restarting the view", async () => {
    const { user } = start();
    await chooseGame(user);
    await user.click(screen.getByRole("radio", { name: "Always" }));
    const deadzone = screen.getByRole("slider", { name: "Left stick deadzone" });
    deadzone.focus();
    await user.keyboard("{ArrowRight>17/}");
    await user.click(screen.getByRole("switch", { name: "Invert right stick Y" }));
    await user.click(screen.getByRole("combobox", { name: "Remap Bottom face button" }));
    await user.click(screen.getByRole("option", { name: "Disable button" }));
    await user.click(screen.getByRole("button", { name: "Save controller profile" }));
    await waitFor(async () => {
      const result = await commands.controllersProfileGet(game.package);
      expect(result.status === "ok" && result.data.mode).toBe("always");
      expect(result.status === "ok" && result.data.mapping.left_stick.deadzone).toBe(25);
      expect(result.status === "ok" && result.data.mapping.right_stick.invert_y).toBe(true);
      expect(result.status === "ok" && result.data.mapping.remap).toEqual([
        { from: "south", to: null },
      ]);
    });
    cleanup();
    const restarted = start();
    await chooseGame(restarted.user);
    expect(screen.getByRole("radio", { name: "Always" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("slider", { name: "Left stick deadzone" })).toHaveValue("25");
    expect(screen.getByRole("switch", { name: "Invert right stick Y" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByRole("combobox", { name: "Remap Bottom face button" })).toHaveTextContent(
      "Disable button",
    );
  });

  it("does not save drafts or change another game's profile", async () => {
    const { user } = start();
    await chooseGame(user);
    await user.click(screen.getByRole("radio", { name: "Never" }));
    expect((await commands.controllersProfileGet(game.package)).status).toBe("ok");
    const before = await commands.controllersProfileGet(game.package);
    expect(before.status === "ok" && before.data.mode).toBe("auto");
    await user.click(screen.getByRole("button", { name: "Save controller profile" }));
    await waitFor(async () => {
      const result = await commands.controllersProfileGet(game.package);
      expect(result.status === "ok" && result.data.mode).toBe("never");
    });
    const other = games.at(1);
    if (!other) throw new Error("Second game missing");
    const result = await commands.controllersProfileGet(other.package);
    expect(result.status === "ok" && result.data.mode).toBe("auto");
  });

  it("moves a stick setting from gamepad events and keeps it bounded", async () => {
    const { user } = start();
    await chooseGame(user);
    await flush();
    const slider = screen.getByRole("slider", { name: "Left stick deadzone" });
    slider.focus();
    await act(async () => {
      await events.uiNav.emit({ action: "right", controller: "xinput", repeat: false });
    });
    expect(slider).toHaveValue("9");
    expect(slider).toHaveFocus();
    for (let i = 0; i < 12; i += 1)
      await act(async () => {
        await events.uiNav.emit({ action: "left", controller: "xinput", repeat: true });
      });
    expect(slider).toHaveValue("0");
  });
});
