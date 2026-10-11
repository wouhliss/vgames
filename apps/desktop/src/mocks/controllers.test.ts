import { describe, expect, it } from "vitest";
import { commands, type PackageRef } from "../ipc";
import { installMockBackend } from "./backend";
import { defaultControllerProfile, MOCK_DUALSENSE } from "./controllers";

const pkg: PackageRef = {
  server_id: "01920000-0000-7000-8000-000000000001",
  package_id: "01920000-0000-7000-8000-000000000002",
};

describe("controller contract mock", () => {
  it("keeps settings and profiles through restart, without persisting connected pads", async () => {
    const persistKey = "controller-contract-restart";
    localStorage.removeItem(persistKey);
    installMockBackend({
      persistKey,
      controllers: { state: { kind: "ready" }, pads: [MOCK_DUALSENSE] },
    });
    expect((await commands.controllersSettingsSet({ nintendo_labels: true })).status).toBe("ok");
    const profile = defaultControllerProfile(true);
    profile.mode = "always";
    profile.mapping.left_stick.deadzone = 25;
    profile.mapping.remap = [{ from: "touchpad", to: "back" }];
    expect((await commands.controllersProfileSet(pkg, profile)).status).toBe("ok");
    installMockBackend({ persistKey });
    expect(await commands.controllersSettingsGet()).toEqual({
      status: "ok",
      data: { nintendo_labels: true },
    });
    expect(await commands.controllersProfileGet(pkg)).toEqual({ status: "ok", data: profile });
    expect(await commands.controllersList()).toEqual({
      status: "ok",
      data: { state: { kind: "ready" }, pads: [] },
    });
    localStorage.removeItem(persistKey);
  });

  it("refuses invalid and duplicate remaps without changing the previous profile", async () => {
    installMockBackend();
    const good = defaultControllerProfile();
    good.mode = "never";
    await commands.controllersProfileSet(pkg, good);
    for (const deadzone of [-1, 100, 0.5, Number.NaN]) {
      const bad = defaultControllerProfile();
      bad.mapping.left_stick.deadzone = deadzone;
      expect((await commands.controllersProfileSet(pkg, bad)).status).toBe("error");
    }
    const bad = defaultControllerProfile();
    bad.mapping.remap = [
      { from: "south", to: "a" },
      { from: "south", to: null },
    ];
    expect((await commands.controllersProfileSet(pkg, bad)).status).toBe("error");
    expect(await commands.controllersProfileGet(pkg)).toEqual({ status: "ok", data: good });
    const other = { ...pkg, server_id: "01920000-0000-7000-8000-000000000003" };
    expect(await commands.controllersProfileGet(other)).toEqual({
      status: "ok",
      data: defaultControllerProfile(),
    });
  });

  it("reports the missing requirements of each host", async () => {
    const backend = installMockBackend();
    for (const [os, kind] of [
      ["windows", "vigembus_missing"],
      ["linux", "uinput_denied"],
      ["macos", "macos_entitlement_missing"],
    ] as const) {
      backend.state.appInfo.os = os;
      const status = await commands.controllersDriverStatus();
      expect(status.status === "ok" && status.data.emulation.kind).toBe(kind);
      if (os === "windows")
        expect(status.status === "ok" && status.data.hiding.kind).toBe("hidhide_missing");
    }
  });

  it("opens testers only for a connected pad and stops idempotently", async () => {
    const backend = installMockBackend({
      controllers: { state: { kind: "ready" }, pads: [MOCK_DUALSENSE] },
    });
    expect((await commands.controllersTesterStart(999)).status).toBe("error");
    const tester = await commands.controllersTesterStart(MOCK_DUALSENSE.instance_id);
    if (tester.status !== "ok") throw new Error("connected tester failed");
    expect(tester.data.instance_id).toBe(MOCK_DUALSENSE.instance_id);
    expect((await commands.controllersTesterStop(tester.data.subscription_id)).status).toBe("ok");
    expect((await commands.controllersTesterStop(tester.data.subscription_id)).status).toBe("ok");
    backend.state.controllers.pads = [];
    expect((await commands.controllersTesterStart(MOCK_DUALSENSE.instance_id)).status).toBe(
      "error",
    );
  });
});
