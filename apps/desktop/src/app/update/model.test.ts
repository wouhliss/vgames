import { describe, expect, it } from "vitest";
import type { UpdaterStatus } from "../../ipc";
import { groupEntries, installBlock, updateVersion } from "./model";

const status = (state: UpdaterStatus["state"], blocked: UpdaterStatus["blocked"] = null) =>
  ({ current_version: "0.4.0", state, blocked }) satisfies UpdaterStatus;

describe("groupEntries", () => {
  it("orders groups New, Improved, Fixed, Removed, Security and drops empty ones", () => {
    const groups = groupEntries([
      { type: "security", text: "S" },
      { type: "fixed", text: "F1" },
      { type: "added", text: "A" },
      { type: "fixed", text: "F2" },
    ]);
    expect(groups).toEqual([
      { type: "added", texts: ["A"] },
      { type: "fixed", texts: ["F1", "F2"] },
      { type: "security", texts: ["S"] },
    ]);
  });

  it("returns nothing for a release without entries", () => {
    expect(groupEntries([])).toEqual([]);
  });
});

describe("installBlock", () => {
  it("blocks while a game runs, but not while downloads pause", () => {
    const available = { kind: "available", version: "0.9.1", date: null } as const;
    expect(installBlock(status(available, "game_running"), false)).toBe("game_running");
    expect(installBlock(status(available, "downloads_active"), false)).toBeNull();
    expect(installBlock(status(available), false)).toBeNull();
  });

  it("blocks while installing", () => {
    const available = { kind: "available", version: "0.9.1", date: null } as const;
    expect(installBlock(status(available), true)).toBe("installing");
    expect(
      installBlock(
        status({ kind: "downloading", version: "0.9.1", downloaded: 1, total: 2 }),
        false,
      ),
    ).toBe("installing");
  });
});

describe("updateVersion", () => {
  it("names the update only when one is known", () => {
    expect(updateVersion(status({ kind: "available", version: "1.0.0", date: null }))).toBe(
      "1.0.0",
    );
    expect(updateVersion(status({ kind: "up_to_date" }))).toBeNull();
    expect(updateVersion(status({ kind: "failed", message: "x" }))).toBeNull();
    expect(updateVersion(undefined)).toBeNull();
  });
});
