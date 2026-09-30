import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import { type BackendStatus, events, type PadProfile } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { BUILT_IN_PROFILE } from "../../mocks/controllers";
import { makeInstalls } from "../../mocks/library";
import { flush, renderWithProviders } from "../../test/render";

const KEY = "vgames.test.controllers";

const INSTALLS = makeInstalls(3, MOCK_SERVER, [MOCK_LIBRARY]).map((install, n) => ({
  ...install,
  state: "installed" as const,
  compat: n === 2 ? ("proton" as const) : ("native" as const),
}));
const NATIVE = INSTALLS[0];
const PROTON = INSTALLS[2];
if (!NATIVE || !PROTON) throw new Error("fixture");

function start(
  controllers: Partial<MockState["controllers"]> = {},
  overrides: Partial<MockState> = {},
) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: INSTALLS,
    persistKey: KEY,
    ...overrides,
  });
  Object.assign(backend.state.controllers, controllers);
  const router = createMemoryRouter(routes, { initialEntries: ["/settings/controllers"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

const region = () => screen.findByRole("region", { name: "Controllers" });

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("connected controllers", () => {
  it("lists each pad with its kind, connection, battery and player", async () => {
    start();
    const section = await region();
    expect(await within(section).findByText("DualSense Wireless Controller")).toBeVisible();
    expect(within(section).getByText("DualSense · Bluetooth · Battery 64%")).toBeVisible();
    expect(within(section).getByText("Xbox · USB")).toBeVisible();
    expect(within(section).getByText("Player 1")).toBeVisible();
    expect(within(section).getByText("Player 2")).toBeVisible();
  });

  it("says so when nothing is connected, and updates when a pad is plugged in", async () => {
    const { backend } = start({ pads: [] });
    const section = await region();
    expect(await within(section).findByText("No controller is connected.")).toBeVisible();
    backend.state.controllers.pads = [
      {
        instance_id: 9,
        kind: "switch_pro",
        name: "Pro Controller",
        connection: "usb",
        battery: { percent: 90, charging: true },
        player: null,
      },
    ];
    await act(async () => {
      await events.controllerEvent.emit({
        instance_id: 9,
        change: { kind: "connected", controller: "switch_pro", name: "Pro Controller" },
      });
      await flush();
    });
    expect(await within(section).findByText("Pro Controller")).toBeVisible();
    expect(within(section).getByText("Switch Pro · USB · Battery 90%, charging")).toBeVisible();
    expect(within(section).queryByText("No controller is connected.")).toBeNull();
  });

  it.each<[string, BackendStatus, RegExp, string | null]>([
    ["ready", { kind: "ready", backend: "uinput" }, /Virtual controllers are ready/, null],
    [
      "driver missing",
      { kind: "driver_missing", backend: "vigem", help_url: "https://example.org/vigem" },
      /ViGEmBus driver isn't installed/,
      "https://example.org/vigem",
    ],
    [
      "no access",
      { kind: "no_permission", backend: "uinput", help_url: "https://example.org/udev" },
      /no access to \/dev\/uinput/,
      "https://example.org/udev",
    ],
    [
      "mac build",
      { kind: "disabled_build", backend: "corehid" },
      /can't create virtual controllers on a Mac/,
      null,
    ],
    ["mac native", { kind: "not_needed" }, /Nothing to set up/, null],
  ])("explains the virtual controller backend: %s", async (_name, backendStatus, text, help) => {
    const { backend, user } = start({ backend: backendStatus });
    const section = await region();
    expect(await within(section).findByText(text)).toBeVisible();
    const button = within(section).queryByRole("button", { name: "How to fix this" });
    if (help === null) {
      expect(button).toBeNull();
      return;
    }
    await user.click(button as HTMLElement);
    expect(backend.callsTo("open_external_url")[0]?.args).toEqual({ url: help });
  });
});

describe("the live tester", () => {
  it("shows what the pad sends, and stops when you stop it or leave", async () => {
    const { backend, user, router } = start();
    const section = await region();
    await user.click(await within(section).findByRole("button", { name: "Start the tester" }));
    expect(backend.callsTo("controller_tester_start")).toHaveLength(1);
    expect(await within(section).findAllByText(/Waiting for input/)).toHaveLength(2);
    await act(async () => {
      await events.controllerInput.emit({
        instance_id: 1,
        pressed: ["south", "dpad_up"],
        left: [1200, -300],
        right: [0, 0],
        left_trigger: 32767,
        right_trigger: 0,
      });
      await new Promise((r) => setTimeout(r, 50));
      await flush();
    });
    expect(
      await within(section).findByText("Pressed: Bottom button (A / Cross), D-pad up"),
    ).toBeVisible();
    expect(within(section).getByText(/Left stick 1,?200, -300/)).toBeVisible();
    expect(within(section).getByRole("progressbar", { name: "Left trigger" })).toHaveAttribute(
      "aria-valuenow",
      "100",
    );
    await user.click(within(section).getByRole("button", { name: "Stop the tester" }));
    expect(backend.callsTo("controller_tester_stop")).toHaveLength(1);
    // Leaving with the tester on stops the stream too.
    await user.click(within(section).getByRole("button", { name: "Start the tester" }));
    await act(async () => {
      await router.navigate("/settings/general");
    });
    await waitFor(() => expect(backend.callsTo("controller_tester_stop")).toHaveLength(2));
  });

  it("says when it can't start", async () => {
    const { backend, user } = start();
    backend.on("controller_tester_start", () => {
      throw Object.assign(new Error("x"), {});
    });
    const section = await region();
    await user.click(await within(section).findByRole("button", { name: "Start the tester" }));
    expect(await within(section).findByText("Couldn't start the tester.")).toBeVisible();
  });
});

describe("per-game settings", () => {
  it("sets the emulation, and it survives a restart", async () => {
    const { backend, user } = start();
    const section = await region();
    const select = await within(section).findByRole("combobox", {
      name: `Virtual controller for ${NATIVE.title}`,
    });
    expect(select).toHaveTextContent("Automatic");
    await user.click(select);
    await user.click(screen.getByRole("option", { name: "Always use a virtual controller" }));
    expect(backend.callsTo("controller_emulation_set")[0]?.args).toEqual({
      package: NATIVE.package,
      mode: "always",
    });
    cleanup();
    start();
    const again = await screen.findByRole("combobox", {
      name: `Virtual controller for ${NATIVE.title}`,
    });
    expect(again).toHaveTextContent("Always use a virtual controller");
  });

  it("a Proton or Wine game has no emulation choice, only a mapping", async () => {
    start();
    const section = await region();
    expect(
      await within(section).findByText("Proton and Wine handle controllers themselves."),
    ).toBeVisible();
    expect(
      within(section).queryByRole("combobox", { name: `Virtual controller for ${PROTON.title}` }),
    ).toBeNull();
    expect(
      within(section).getByRole("button", {
        name: `Edit the controller mapping of ${PROTON.title}`,
      }),
    ).toBeVisible();
  });

  it("shows a failure to save", async () => {
    const { user } = start({ errors: { controller_emulation_set: { kind: "not_found" } } });
    const section = await region();
    await user.click(
      await within(section).findByRole("combobox", {
        name: `Virtual controller for ${NATIVE.title}`,
      }),
    );
    await user.click(screen.getByRole("option", { name: "Never use a virtual controller" }));
    expect(await screen.findByText("Couldn't save the controller setting.")).toBeVisible();
  });
});

describe("mapping profiles", () => {
  const open = async (user: ReturnType<typeof userEvent.setup>, name: string | RegExp) => {
    const section = await region();
    await user.click(await within(section).findByRole("button", { name }));
    return screen.findByRole("dialog");
  };

  it("edits the default mapping: numbers, invert, remaps; saved and kept after a restart", async () => {
    const { backend, user } = start();
    const dialog = await open(user, "Edit the default mapping");
    const deadzones = within(dialog).getAllByRole("textbox", { name: /^Deadzone/ });
    await user.clear(deadzones[0] as HTMLElement);
    await user.type(deadzones[0] as HTMLElement, "15");
    const inverts = within(dialog).getAllByRole("switch", { name: "Invert up and down" });
    await user.click(inverts[1] as HTMLElement);
    await user.click(within(dialog).getByRole("switch", { name: "Use Nintendo button labels" }));
    await user.click(within(dialog).getByRole("button", { name: "Change a button" }));
    await user.click(within(dialog).getByRole("button", { name: "Save mapping" }));
    await waitFor(() => expect(backend.callsTo("controller_profile_set")).toHaveLength(1));
    const saved = backend.callsTo("controller_profile_set")[0]?.args as {
      package: unknown;
      profile: PadProfile;
    };
    expect(saved.package).toBeNull();
    expect(saved.profile.nintendo_labels).toBe(true);
    expect(saved.profile.left_stick.deadzone).toBe(15);
    expect(saved.profile.right_stick.invert_y).toBe(true);
    expect(saved.profile.remap).toEqual([{ from: "south", to: null }]);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    cleanup();
    start();
    const again = await open(userEvent.setup(), "Edit the default mapping");
    expect(within(again).getAllByRole("textbox", { name: /^Deadzone/ })[0]).toHaveValue("15");
    expect(within(again).getByText("Bottom button (A / Cross)")).toBeVisible();
  });

  it.each(["", "abc", "100", "-1", "1.5", "150"])(
    "refuses %j as a deadzone without sending it",
    async (text) => {
      const { backend, user } = start();
      const dialog = await open(user, "Edit the default mapping");
      const field = within(dialog).getAllByRole("textbox", { name: /^Deadzone/ })[0] as HTMLElement;
      await user.clear(field);
      if (text) await user.type(field, text);
      await user.click(within(dialog).getByRole("button", { name: "Save mapping" }));
      expect(await within(dialog).findByText("Enter a whole number from 0 to 99.")).toBeVisible();
      expect(backend.callsTo("controller_profile_set")).toHaveLength(0);
    },
  );

  it("accepts the boundaries 0 and 99", async () => {
    const { backend, user } = start();
    const dialog = await open(user, "Edit the default mapping");
    const zones = within(dialog).getAllByRole("textbox", { name: /deadzone/i });
    await user.clear(zones[0] as HTMLElement);
    await user.type(zones[0] as HTMLElement, "0");
    await user.clear(zones[1] as HTMLElement);
    await user.type(zones[1] as HTMLElement, "99");
    await user.click(within(dialog).getByRole("button", { name: "Save mapping" }));
    await waitFor(() => expect(backend.callsTo("controller_profile_set")).toHaveLength(1));
  });

  it("removes a change and never offers a button twice", async () => {
    const { user } = start({});
    const dialog = await open(user, "Edit the default mapping");
    await user.click(within(dialog).getByRole("button", { name: "Change a button" }));
    await user.click(within(dialog).getByRole("button", { name: "Change a button" }));
    const rows = within(dialog).getAllByRole("combobox", { name: "When you press" });
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent("Bottom button");
    expect(rows[1]).toHaveTextContent("Right button");
    await user.click(rows[1] as HTMLElement);
    expect(screen.queryByRole("option", { name: "Bottom button (A / Cross)" })).toBeNull();
    await user.keyboard("{Escape}");
    await user.click(
      within(dialog).getByRole("button", {
        name: "Remove the change for Right button (B / Circle)",
      }),
    );
    expect(within(dialog).getAllByRole("combobox", { name: "When you press" })).toHaveLength(1);
  });

  it("a game's mapping is saved for that game, and 'Use the default mapping' resets it", async () => {
    const { backend, user } = start();
    const dialog = await open(user, `Edit the controller mapping of ${NATIVE.title}`);
    await user.click(within(dialog).getByRole("switch", { name: "Use Nintendo button labels" }));
    await user.click(within(dialog).getByRole("button", { name: "Save mapping" }));
    await waitFor(() => expect(backend.callsTo("controller_profile_set")).toHaveLength(1));
    expect(backend.callsTo("controller_profile_set")[0]?.args).toMatchObject({
      package: NATIVE.package,
    });
    const section = await region();
    expect(await within(section).findByText("Custom mapping")).toBeVisible();
    const again = await open(user, `Edit the controller mapping of ${NATIVE.title}`);
    await user.click(within(again).getByRole("button", { name: "Use the default mapping" }));
    await waitFor(() => expect(backend.callsTo("controller_profile_reset")).toHaveLength(1));
    expect(backend.callsTo("controller_profile_reset")[0]?.args).toEqual({
      package: NATIVE.package,
    });
    await waitFor(() => expect(within(section).queryByText("Custom mapping")).toBeNull());
  });

  it("shows the core's refusal and keeps the dialog", async () => {
    const { user } = start({ errors: { controller_profile_set: { kind: "range" } } });
    const dialog = await open(user, "Edit the default mapping");
    await user.click(within(dialog).getByRole("button", { name: "Save mapping" }));
    expect(await within(dialog).findByText("Deadzones must be between 0 and 99.")).toBeVisible();
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("copies the mapping as text and loads pasted text without saving it", async () => {
    const { backend, user } = start();
    const dialog = await open(user, "Edit the default mapping");
    await user.click(within(dialog).getByRole("button", { name: "Copy as text" }));
    expect(JSON.parse(await navigator.clipboard.readText())).toEqual(BUILT_IN_PROFILE);
    await user.click(within(dialog).getByRole("button", { name: "Paste a mapping" }));
    const pasted: PadProfile = {
      nintendo_labels: true,
      left_stick: { deadzone: 20, anti_deadzone: 5, invert_y: true },
      right_stick: { deadzone: 8, anti_deadzone: 0, invert_y: false },
      remap: [{ from: "east", to: "a" }],
    };
    const box = within(dialog).getByRole("textbox", { name: "Mapping text" });
    await user.click(box);
    await user.paste(JSON.stringify(pasted));
    await user.click(within(dialog).getByRole("button", { name: "Use this mapping" }));
    expect(await screen.findByText("Mapping loaded. Save to keep it.")).toBeVisible();
    expect(within(dialog).getAllByRole("textbox", { name: /^Deadzone/ })[0]).toHaveValue("20");
    expect(within(dialog).getByText("Right button (B / Circle)")).toBeVisible();
    expect(backend.callsTo("controller_profile_set")).toHaveLength(0);
  });

  it.each([
    ["not json at all", /isn't a controller mapping/],
    ['{"remap": 3}', /isn't a controller mapping: missing fields/],
    [
      JSON.stringify({
        ...BUILT_IN_PROFILE,
        left_stick: { deadzone: 100, anti_deadzone: 0, invert_y: false },
      }),
      /Deadzones must be between 0 and 99/,
    ],
    [
      JSON.stringify({
        ...BUILT_IN_PROFILE,
        remap: [
          { from: "south", to: "a" },
          { from: "south", to: "b" },
        ],
      }),
      /Bottom button \(A \/ Cross\) is changed twice/,
    ],
  ])("refuses pasted text %s", async (text, message) => {
    const { user } = start();
    const dialog = await open(user, "Edit the default mapping");
    await user.click(within(dialog).getByRole("button", { name: "Paste a mapping" }));
    await user.click(within(dialog).getByRole("textbox", { name: "Mapping text" }));
    await user.paste(text);
    await user.click(within(dialog).getByRole("button", { name: "Use this mapping" }));
    expect(await within(dialog).findByText(message)).toBeVisible();
  });
});
