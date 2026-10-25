import { describe, expect, it } from "vitest";
import { commands, type PackageRef } from "../ipc";
import { installMockBackend } from "./backend";
import { makeSaveConflict, makeSaveHistory } from "./saves";

const pkg: PackageRef = {
  server_id: "01920000-0000-7000-8000-000000000001",
  package_id: "01920000-0000-7000-8000-000000000002",
};

describe("cloud-save mock", () => {
  it("keeps history, restores and resolved conflicts through a restart", async () => {
    const persistKey = "saves-contract-restart";
    localStorage.removeItem(persistKey);
    const conflict = makeSaveConflict(pkg);
    installMockBackend({
      persistKey,
      saveConflicts: { [conflict.conflict_id]: conflict },
      saveHistories: { [pkg.package_id]: makeSaveHistory(pkg) },
    });
    const older = makeSaveHistory(pkg).entries[2];
    if (!older) throw new Error("fixture");
    expect((await commands.savesRestore(pkg, older.source)).status).toBe("ok");
    expect((await commands.saveConflictResolve(conflict.conflict_id, "keep_cloud")).status).toBe(
      "ok",
    );

    // Restart: the same storage, nothing passed in.
    installMockBackend({ persistKey });
    const history = await commands.savesHistory(pkg);
    if (history.status !== "ok") throw new Error("history");
    // The restore's own backup of the files it replaced comes first.
    expect(history.data.entries).toHaveLength(4);
    expect(history.data.entries[0]?.source.kind).toBe("backup");
    expect((await commands.saveConflictGet(conflict.conflict_id)).status).toBe("error");
    expect(await commands.savesStatus(pkg)).toMatchObject({
      status: "ok",
      data: { state: "synced", conflict_id: null },
    });
    localStorage.removeItem(persistKey);
  });
});
