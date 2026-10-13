import { afterEach, describe, expect, it } from "vitest";
import { type ConflictChoice, commands, type PackageRef } from "../ipc";
import { installMockBackend } from "./backend";
import { conflictFixture, saveKey } from "./saves";

const pkg: PackageRef = {
  server_id: "01920000-0000-7000-8000-000000000001",
  package_id: "01920000-0000-7000-8000-000000000002",
};
afterEach(() => localStorage.removeItem("saves-fixture-restart"));
describe("pending cloud-save interfaces", () => {
  it.each<ConflictChoice>(["keep_cloud", "keep_device", "keep_both", "cancel"])(
    "keeps the explicit %s choice and history through restart",
    async (choice) => {
      const entry = conflictFixture(pkg);
      const id = entry.status.conflict_id ?? "";
      installMockBackend({
        persistKey: "saves-fixture-restart",
        saveRecords: { [saveKey(pkg)]: entry },
      });
      expect((await commands.saveConflictGet(id)).status).toBe("ok");
      const result = await commands.saveConflictResolve(id, choice);
      expect(result.status).toBe("ok");
      installMockBackend({ persistKey: "saves-fixture-restart" });
      expect(await commands.savesStatus(pkg)).toEqual(result);
      const history = await commands.savesHistory(pkg);
      if (history.status !== "ok") throw new Error("Missing history");
      expect(history.data.entries).toHaveLength(
        choice === "keep_both" ? 4 : choice === "keep_device" ? 3 : 2,
      );
      expect((await commands.saveConflictGet(id)).status).toBe(
        choice === "cancel" ? "ok" : "error",
      );
    },
  );
  it("refuses wrong-package restore sources, running games and failed uploads without clearing conflict", async () => {
    const entry = conflictFixture(pkg);
    const backend = installMockBackend({ saveRecords: { [saveKey(pkg)]: entry } });
    const id = entry.status.conflict_id ?? "";
    expect(
      (
        await commands.savesRestore(
          { ...pkg, server_id: "another-server" },
          entry.history.entries[0]?.source ?? { kind: "cloud", snapshot_id: "missing" },
        )
      ).status,
    ).toBe("error");
    entry.running = true;
    expect((await commands.saveConflictResolve(id, "keep_cloud")).status).toBe("error");
    entry.running = false;
    entry.operationError = {
      kind: "server",
      code: "save_quota_exceeded",
      message: "Cloud storage is full.",
    };
    expect((await commands.saveConflictResolve(id, "keep_device")).status).toBe("error");
    expect(backend.state.saveRecords[saveKey(pkg)]?.status.state).toBe("conflict");
    entry.operationError = null;
    const source = entry.history.entries.find((item) => item.source.kind === "backup")?.source;
    if (!source) throw new Error("Missing backup");
    expect((await commands.savesRestore(pkg, source)).status).toBe("ok");
  });
  it("reports unsupported and offline-pending packages without inventing a conflict", async () => {
    installMockBackend();
    expect(await commands.savesStatus(pkg)).toMatchObject({
      status: "ok",
      data: { state: "unsupported", conflict_id: null },
    });
    const entry = conflictFixture(pkg);
    entry.status.state = "pending";
    entry.status.conflict_id = null;
    entry.conflict = null;
    installMockBackend({ saveRecords: { [saveKey(pkg)]: entry } });
    expect(await commands.savesStatus(pkg)).toMatchObject({
      status: "ok",
      data: { state: "pending", conflict_id: null },
    });
  });
});
