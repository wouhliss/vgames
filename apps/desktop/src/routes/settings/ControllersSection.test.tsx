import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import { type ControllerDriverState, type ControllerTester, commands, events } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { MOCK_DUALSENSE } from "../../mocks/controllers";
import { flush, renderWithProviders } from "../../test/render";

const KEY = "controller-screen-restart";
function start(overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({
    persistKey: KEY,
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    installs: [],
    controllers: { state: { kind: "ready" }, pads: [MOCK_DUALSENSE] },
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: ["/settings/controllers"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, user: userEvent.setup() };
}

beforeEach(() => localStorage.clear());

describe("Controllers settings", () => {
  it("shows pads and persists Nintendo labels through restart with keyboard controls", async () => {
    const { backend, user } = start();
    expect(
      await screen.findByRole("heading", { level: 4, name: MOCK_DUALSENSE.name }),
    ).toBeVisible();
    const labels = await screen.findByRole("switch", { name: "Use Nintendo button labels" });
    labels.focus();
    await user.keyboard(" ");
    await waitFor(() => expect(backend.state.controllerSettings.nintendo_labels).toBe(true));
    cleanup();
    start();
    expect(
      await screen.findByRole("switch", { name: "Use Nintendo button labels" }),
    ).toHaveAttribute("aria-checked", "true");
  });

  it("shows empty, unavailable and retry states", async () => {
    start({ controllers: { state: { kind: "ready" }, pads: [] } });
    expect(await screen.findByText("No controllers connected")).toBeVisible();
    cleanup();
    start({
      controllers: {
        state: { kind: "unavailable", detail: "Controller access denied." },
        pads: [],
      },
    });
    expect(await screen.findByText("Controller access denied.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
  });

  it.each([
    [{ kind: "available" }, "Available"],
    [{ kind: "not_required" }, "Not required on this platform"],
    [{ kind: "vigembus_missing" }, "ViGEmBus"],
    [{ kind: "hidhide_missing" }, "HidHide"],
    [
      { kind: "uinput_denied", udev_rule: 'KERNEL=="uinput", TAG+="uaccess"' },
      'KERNEL=="uinput", TAG+="uaccess"',
    ],
    [{ kind: "macos_entitlement_missing" }, /Virtual controllers are unavailable/],
    [{ kind: "probe_error", detail: "Driver not responding." }, /Driver not responding/],
  ] satisfies [ControllerDriverState, string | RegExp][])(
    "explains driver state %j",
    async (state, text) => {
      start({ controllerDrivers: { emulation: state, hiding: { kind: "not_required" } } });
      expect((await screen.findAllByText(text)).length).toBeGreaterThan(0);
    },
  );

  it("filters tester events and stops the subscription when the pad disconnects", async () => {
    const { backend, user } = start();
    await user.click(await screen.findByRole("button", { name: `Test ${MOCK_DUALSENSE.name}` }));
    const dialog = await screen.findByRole("dialog", {
      name: `Test controller: ${MOCK_DUALSENSE.name}`,
    });
    await waitFor(() => expect(backend.callsTo("controllers_tester_start")).toHaveLength(1));
    await flush();
    const event = {
      subscription_id: "controller-tester-1",
      instance_id: MOCK_DUALSENSE.instance_id,
      buttons: ["south"] as const,
      left: [32767, 0] as [number, number],
      right: [0, 0] as [number, number],
      left_trigger: 0,
      right_trigger: 0,
    };
    await act(async () => {
      await events.controllerTesterState.emit({
        ...event,
        buttons: [...event.buttons],
        subscription_id: "other",
      });
    });
    expect(within(dialog).queryByText("Bottom face button")).not.toBeInTheDocument();
    await act(async () => {
      await events.controllerTesterState.emit({ ...event, buttons: [...event.buttons] });
    });
    expect(await within(dialog).findByText("Bottom face button")).toBeVisible();
    expect(within(dialog).getByText("100%, 0%")).toBeVisible();
    backend.state.controllers.pads = [];
    await act(async () => {
      await events.padsChanged.emit({});
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    await waitFor(() => expect(backend.callsTo("controllers_tester_stop")).toHaveLength(1));
  });

  it("keeps the initial tester state emitted before the start reply", async () => {
    const { backend, user } = start();
    backend.on("controllers_tester_start", async () => {
      await events.controllerTesterState.emit({
        subscription_id: "early",
        instance_id: MOCK_DUALSENSE.instance_id,
        buttons: ["south"],
        left: [0, 0],
        right: [0, 0],
        left_trigger: 0,
        right_trigger: 0,
      });
      return { subscription_id: "early", instance_id: MOCK_DUALSENSE.instance_id };
    });
    await user.click(await screen.findByRole("button", { name: `Test ${MOCK_DUALSENSE.name}` }));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText("Bottom face button")).toBeVisible();
  });

  it("releases a tester that finishes starting after its dialog closes", async () => {
    const { backend, user } = start();
    let resolve:
      | ((value: Awaited<ReturnType<typeof commands.controllersTesterStart>>) => void)
      | undefined;
    vi.spyOn(commands, "controllersTesterStart").mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    await user.click(await screen.findByRole("button", { name: `Test ${MOCK_DUALSENSE.name}` }));
    await waitFor(() => expect(resolve).toBeDefined());
    const dialog = screen.getByRole("dialog");
    const close = within(dialog).getAllByRole("button", { name: "Close" }).at(0);
    if (!close) throw new Error("Close button missing");
    await user.click(close);
    await act(async () => {
      resolve?.({
        status: "ok",
        data: {
          subscription_id: "late",
          instance_id: MOCK_DUALSENSE.instance_id,
        } satisfies ControllerTester,
      });
    });
    await waitFor(() =>
      expect(backend.callsTo("controllers_tester_stop")[0]?.args).toEqual({
        subscriptionId: "late",
      }),
    );
  });

  it("has no axe violations and opens/closes the tester from gamepad events", async () => {
    start();
    const test = await screen.findByRole("button", { name: `Test ${MOCK_DUALSENSE.name}` });
    await flush();
    expect(
      (await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } }))
        .violations,
    ).toEqual([]);
    test.focus();
    await act(async () => {
      await events.uiNav.emit({ action: "accept", controller: "dualsense", repeat: false });
    });
    expect(await screen.findByRole("dialog")).toBeVisible();
    await flush();
    await act(async () => {
      await events.uiNav.emit({ action: "back", controller: "dualsense", repeat: false });
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });
});
