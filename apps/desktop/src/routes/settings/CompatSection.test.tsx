import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent, { type UserEvent } from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import type { CompatLayer } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { makeInstalls } from "../../mocks/library";
import { renderWithProviders } from "../../test/render";

const KEY = "vgames.test.compat";

function games(layer: CompatLayer) {
  return makeInstalls(3, MOCK_SERVER, [MOCK_LIBRARY]).map((install, n) => ({
    ...install,
    compat: n === 2 ? ("native" as const) : layer,
  }));
}

const LINUX = games("proton");
const MAC = games("wine");

function start(overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: LINUX,
    persistKey: KEY,
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: ["/settings/compatibility"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, user: userEvent.setup() };
}

/** Closes the app and starts it again on the same (mock) storage. */
function restart(overrides: Partial<MockState> = {}) {
  cleanup();
  return start(overrides);
}

const region = () => screen.findByRole("region", { name: "Compatibility" });

async function pick(user: UserEvent, label: string | RegExp, option: RegExp) {
  await user.click(await screen.findByRole("combobox", { name: label }));
  await user.click(screen.getByRole("option", { name: option }));
}

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("compatibility settings", () => {
  describe("on Linux", () => {
    it("shows Proton, the downloaded runtimes with their size, and no Rosetta", async () => {
      start();
      const section = await region();
      expect(await within(section).findByText(/through Proton/)).toBeVisible();
      expect(
        within(section).getByRole("combobox", { name: /Default Proton version/ }),
      ).toHaveTextContent("Automatic (recommended)");
      expect(within(section).getByText("UMU-Proton 9.0-4")).toBeVisible();
      expect(within(section).getByText("GE-Proton9-27")).toBeVisible();
      expect(within(section).getByText("2.7 GB in total.")).toBeVisible();
      // Only a runtime no game uses can be removed.
      expect(within(section).getByRole("button", { name: "Remove GE-Proton9-27" })).toBeVisible();
      expect(within(section).queryByRole("button", { name: "Remove UMU-Proton 9.0-4" })).toBeNull();
      expect(within(section).getAllByText(/Used by 3 games/)).toHaveLength(2);
      expect(within(section).queryByText("Rosetta 2")).toBeNull();
    });

    it("keeps the default Proton version after a restart", async () => {
      const { user, backend } = start();
      await pick(user, /Default Proton version/, /^GE-Proton10-4/);
      await waitFor(() =>
        expect(backend.state.defaultRunner).toEqual({
          runtime: "ge-proton",
          version: "GE-Proton10-4",
        }),
      );

      const again = restart().user;
      expect(
        await screen.findByRole("combobox", { name: /Default Proton version/ }),
      ).toHaveTextContent("GE-Proton10-4");
      // A game's own choice names the default it follows.
      await again.click(
        await screen.findByRole("button", { name: `Customize how ${LINUX[0]?.title} runs` }),
      );
      expect(await screen.findByRole("combobox", { name: "Proton version" })).toHaveTextContent(
        "Default (GE-Proton10-4)",
      );
    });

    it("removes an unused runtime and keeps focus in the list", async () => {
      const { user, backend } = start();
      await user.click(await screen.findByRole("button", { name: "Remove GE-Proton9-27" }));
      expect(
        await screen.findByText("Removed GE-Proton9-27. It downloads again if a game needs it."),
      ).toBeVisible();
      await waitFor(() => expect(screen.queryByText("GE-Proton9-27")).toBeNull());
      expect(screen.getByRole("heading", { name: "Downloaded runtimes" })).toHaveFocus();
      expect(backend.state.runtimes.some((r) => r.version === "GE-Proton9-27")).toBe(false);

      restart();
      expect(await screen.findByText("UMU-Proton 9.0-4")).toBeVisible();
      expect(screen.queryByText("GE-Proton9-27")).toBeNull();
    });

    it("says why a runtime is still needed when the core refuses to remove it", async () => {
      const { user, backend } = start();
      const button = await screen.findByRole("button", { name: "Remove GE-Proton9-27" });
      // A game started using it after the list loaded.
      backend.state.runtimes = backend.state.runtimes.map((r) =>
        r.version === "GE-Proton9-27" ? { ...r, used_by: 1 } : r,
      );
      await user.click(button);
      expect(await screen.findByText("GE-Proton9-27 is still used by 1 game.")).toBeVisible();
    });

    it("customizes one game, keeps it after a restart, and resets it", async () => {
      const [game] = LINUX;
      if (!game) throw new Error("fixture");
      const { user, backend } = start();
      // Only games that run through Proton are listed.
      expect(
        await screen.findByRole("button", { name: `Customize how ${game.title} runs` }),
      ).toBeVisible();
      expect(
        screen.queryByRole("button", { name: `Customize how ${LINUX[2]?.title} runs` }),
      ).toBeNull();

      await user.click(screen.getByRole("button", { name: `Customize how ${game.title} runs` }));
      const dialog = await screen.findByRole("dialog", { name: `How ${game.title} runs` });
      expect(within(dialog).queryByRole("combobox", { name: "Graphics" })).toBeNull();
      await pick(user, "Proton version", /^GE-Proton10-4/);
      await user.type(
        within(dialog).getByRole("textbox", { name: /Extra environment variables/ }),
        "PROTON_USE_WINED3D=1",
      );
      await user.click(within(dialog).getByRole("button", { name: "Save" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      expect(
        await screen.findByText("Customized: GE-Proton10-4 · 1 environment variable"),
      ).toBeVisible();
      expect(Object.values(backend.state.compatOverrides)).toEqual([
        {
          runner: { runtime: "ge-proton", version: "GE-Proton10-4" },
          graphics: null,
          env: { PROTON_USE_WINED3D: "1" },
        },
      ]);

      const again = restart().user;
      expect(
        await screen.findByText("Customized: GE-Proton10-4 · 1 environment variable"),
      ).toBeVisible();
      await again.click(
        screen.getByRole("button", { name: `Reset ${game.title} to the defaults` }),
      );
      expect(await screen.findByText(`${game.title} uses the defaults again`)).toBeVisible();
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: `Customize how ${game.title} runs` }),
        ).toHaveFocus(),
      );
      expect(screen.getAllByText("Uses the defaults")).toHaveLength(2);
    });

    it("checks each environment line before saving, and shows the core's refusal", async () => {
      const [game] = LINUX;
      if (!game) throw new Error("fixture");
      const { user, backend } = start();
      await user.click(
        await screen.findByRole("button", { name: `Customize how ${game.title} runs` }),
      );
      const dialog = await screen.findByRole("dialog");
      const env = within(dialog).getByRole("textbox", { name: /Extra environment variables/ });

      await user.type(env, "DXVK_HUD=fps{Enter}{Enter}lowercase=1");
      await user.click(within(dialog).getByRole("button", { name: "Save" }));
      expect(env).toHaveAccessibleDescription(/Line 3 isn't NAME=value/);
      expect(backend.callsTo("compat_override_set")).toHaveLength(0);

      await user.clear(env);
      await user.type(env, "A=1{Enter}A=2");
      await user.click(within(dialog).getByRole("button", { name: "Save" }));
      expect(env).toHaveAccessibleDescription(/Line 2 sets A a second time/);

      // The Rust core has the last word: reserved names are refused there.
      await user.clear(env);
      await user.type(env, "LD_PRELOAD=/tmp/x.so");
      await user.click(within(dialog).getByRole("button", { name: "Save" }));
      expect(
        await within(dialog).findByText(
          "LD_PRELOAD can't be changed here, to keep games from running other programs.",
        ),
      ).toBeVisible();
      expect(backend.state.compatOverrides).toEqual({});
      expect(screen.getByRole("dialog")).toBeVisible();
    });

    it("saving the defaults removes the override", async () => {
      const [game] = LINUX;
      if (!game) throw new Error("fixture");
      const key = `${game.package.server_id}/${game.package.package_id}`;
      const { user, backend } = start({
        compatOverrides: { [key]: { runner: null, graphics: null, env: { DXVK_HUD: "fps" } } },
      });
      await user.click(
        await screen.findByRole("button", { name: `Customize how ${game.title} runs` }),
      );
      const dialog = await screen.findByRole("dialog");
      await user.clear(
        within(dialog).getByRole("textbox", { name: /Extra environment variables/ }),
      );
      await user.click(within(dialog).getByRole("button", { name: "Save" }));
      await waitFor(() => expect(backend.state.compatOverrides).toEqual({}));
      expect(backend.callsTo("compat_override_reset")).toHaveLength(1);
    });

    it("offers a retry when the core can't answer", async () => {
      const { user, backend } = start();
      let failing = true;
      const real = backend.state;
      backend.on("compat_overview", () => {
        if (failing) throw new Error("core unavailable");
        return {
          host: { kind: "proton" },
          runtimes: [],
          runners: [],
          graphics: [],
          default_runner: real.defaultRunner,
        };
      });
      const section = await region();
      const retry = await within(section).findByRole("button", { name: /Try again/ });
      failing = false;
      await user.click(retry);
      expect(await within(section).findByText(/Nothing downloaded yet/)).toBeVisible();
    });
  });

  describe("on a Mac", () => {
    it("with Apple silicon: installs Rosetta 2 after asking, and warns about Apple's plans", async () => {
      const { user, backend } = start({
        host: "macos-aarch64",
        rosettaInstalled: false,
        installs: MAC,
      });
      const section = await region();
      expect(await within(section).findByText(/through Wine/)).toBeVisible();
      expect(within(section).getByText(/Rosetta 2 isn't installed/)).toBeVisible();
      expect(
        within(section).getByText(/Apple plans to limit Rosetta 2 after macOS 27/),
      ).toBeVisible();

      await user.click(within(section).getByRole("button", { name: "Install Rosetta 2" }));
      const confirm = await screen.findByRole("alertdialog", { name: "Install Rosetta 2?" });
      expect(backend.callsTo("rosetta_install")).toHaveLength(0);
      await user.click(within(confirm).getByRole("button", { name: "Install Rosetta 2" }));
      expect(await within(section).findByText("Rosetta 2 is installed.")).toBeVisible();
      expect(backend.callsTo("rosetta_install")).toHaveLength(1);
    });

    it("with Apple silicon: picks a graphics backend per game, D3DMetal included", async () => {
      const [game] = MAC;
      if (!game) throw new Error("fixture");
      const { user, backend } = start({ host: "macos-aarch64", installs: MAC });
      await user.click(
        await screen.findByRole("button", { name: `Customize how ${game.title} runs` }),
      );
      const dialog = await screen.findByRole("dialog");
      expect(within(dialog).getByRole("combobox", { name: "Wine version" })).toHaveTextContent(
        "Default (automatic)",
      );
      await pick(user, "Graphics", /^DXMT/);
      await user.click(within(dialog).getByRole("button", { name: "Save" }));
      expect(await screen.findByText("Customized: DXMT")).toBeVisible();
      expect(Object.values(backend.state.compatOverrides)[0]?.graphics).toBe("dxmt");

      await user.click(screen.getByRole("button", { name: `Customize how ${game.title} runs` }));
      await user.click(await screen.findByRole("combobox", { name: "Graphics" }));
      expect(screen.getByRole("option", { name: /^D3DMetal/ })).toBeVisible();
    });

    it("on an Intel Mac: no Rosetta 2 section and no D3DMetal", async () => {
      const [game] = MAC;
      if (!game) throw new Error("fixture");
      const { user } = start({ host: "macos-x86_64", installs: MAC });
      const section = await region();
      expect(await within(section).findByText(/through Wine/)).toBeVisible();
      expect(within(section).queryByText("Rosetta 2")).toBeNull();
      await user.click(
        await within(section).findByRole("button", { name: `Customize how ${game.title} runs` }),
      );
      await user.click(await screen.findByRole("combobox", { name: "Graphics" }));
      expect(screen.getByRole("option", { name: /^DXMT/ })).toBeVisible();
      expect(screen.queryByRole("option", { name: /^D3DMetal/ })).toBeNull();
    });

    it("shows Apple's D3DMetal license as plain text", async () => {
      const { user, backend } = start({ host: "macos-aarch64", installs: MAC });
      backend.state.runtimeLicenses = backend.state.runtimeLicenses.map((l) =>
        l.runtime === "d3dmetal" ? { ...l, text: "<b>Apple</b> license text" } : l,
      );
      await user.click(await screen.findByRole("button", { name: "Runtime licenses" }));
      const dialog = await screen.findByRole("dialog", { name: "Runtime licenses" });
      const d3dmetal = await within(dialog).findByRole("region", {
        name: "D3DMetal (Apple Game Porting Toolkit)",
      });
      expect(within(d3dmetal).getByText("LicenseRef-Apple-GPTK")).toBeVisible();
      expect(within(d3dmetal).getByText("<b>Apple</b> license text")).toBeVisible();
      expect(dialog.querySelector("b")).toBeNull();
    });
  });

  it("on Windows: nothing to set up", async () => {
    start({ host: "windows-x86_64" });
    const section = await region();
    expect(await within(section).findByText(/There's nothing to set up/)).toBeVisible();
    expect(within(section).queryByRole("combobox")).toBeNull();
  });
});
