import { afterEach, describe, expect, it, vi } from "vitest";
import { buildDiagnostics, describeError } from "./ErrorBoundary";

vi.mock("../ipc", () => ({
  commands: { appDiagnostics: vi.fn(async () => "vgames: 0.1.0\ndatabase: ready") },
}));
afterEach(() => {
  window.history.replaceState(null, "", "/");
});

describe("private crash diagnostics", () => {
  it("includes a known error class and route category without raw messages or identifiers", async () => {
    window.history.replaceState(null, "", "/servers/01920000-0000-7000-8000-000000000001/settings");
    const error = new TypeError(
      "token=vga_abcdefghijklmnop https://example.test/?signature=private /home/player/save",
    );
    const text = await buildDiagnostics(error);
    expect(text).toContain("database: ready");
    expect(text).toContain("ui error: TypeError");
    expect(text).toContain("ui route: servers");
    for (const secret of [
      "abcdefghijklmnop",
      "signature",
      "private",
      "/home/player",
      "01920000-0000-7000-8000-000000000001",
    ])
      expect(text).not.toContain(secret);
  });
  it("keeps custom error names and unknown routes out of the report", async () => {
    const error = new Error("private message");
    error.name = "vga_abcdefghijklmnop";
    expect(describeError(error)).toBe("Error");
    window.history.replaceState(null, "", "/private-account-identifier");
    const text = await buildDiagnostics(error);
    expect(text).toContain("ui route: other");
    expect(text).not.toContain("private-account");
    expect(text).not.toContain("abcdefghijklmnop");
  });
});
