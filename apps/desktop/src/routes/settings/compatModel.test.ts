import { describe, expect, it } from "vitest";
import { formatEnv, overrideSummary, parseEnv, runtimeLabel } from "./compatModel";

describe("compatibility helpers", () => {
  it("names runtimes without repeating GE-Proton's name", () => {
    expect(runtimeLabel("umu-proton", "10.0-2")).toBe("UMU-Proton 10.0-2");
    expect(runtimeLabel("ge-proton", "GE-Proton10-4")).toBe("GE-Proton10-4");
    expect(runtimeLabel("wine-macos", "10.4")).toBe("Wine 10.4");
  });

  it("parses NAME=value lines, skipping blank ones and keeping '=' in values", () => {
    expect(parseEnv("DXVK_HUD=fps\n\n  WINEDEBUG = -all \nOPTS=a=b\n")).toEqual({
      ok: true,
      env: { DXVK_HUD: "fps", WINEDEBUG: "-all", OPTS: "a=b" },
    });
    expect(parseEnv("")).toEqual({ ok: true, env: {} });
  });

  it("points at the first bad line", () => {
    expect(parseEnv("A=1\nlower=1")).toEqual({
      ok: false,
      line: 2,
      problem: "format",
      key: "lower",
    });
    expect(parseEnv("NO_EQUALS")).toMatchObject({ ok: false, line: 1, problem: "format" });
    expect(parseEnv("1ABC=x")).toMatchObject({ ok: false, problem: "format" });
    expect(parseEnv(`${"A".repeat(65)}=x`)).toMatchObject({ ok: false, problem: "format" });
    expect(parseEnv("A=1\nB=2\nA=3")).toEqual({
      ok: false,
      line: 3,
      problem: "duplicate",
      key: "A",
    });
  });

  it("formats back to the lines it parses", () => {
    const env = { DXVK_HUD: "fps", OPTS: "a=b" };
    const parsed = parseEnv(formatEnv(env));
    expect(parsed).toEqual({ ok: true, env });
  });

  it("summarizes an override in one line", () => {
    expect(overrideSummary(null)).toBe("Uses the defaults");
    expect(overrideSummary({ runner: null, graphics: null, env: {} })).toBe("Uses the defaults");
    expect(
      overrideSummary({
        runner: { runtime: "wine-macos", version: "10.4" },
        graphics: "d3dmetal",
        env: { A: "1", B: "2" },
      }),
    ).toBe("Customized: Wine 10.4 · D3DMetal · 2 environment variables");
  });
});
