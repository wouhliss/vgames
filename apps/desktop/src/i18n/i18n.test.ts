import { describe, expect, it } from "vitest";
import { formatBytes, formatDuration, t } from "./index";

describe("t", () => {
  it("returns plain messages", () => {
    expect(t("common.cancel")).toBe("Cancel");
  });
  it("interpolates params", () => {
    expect(t("onboarding.signIn.title", { server: "Friday Night Games" })).toBe(
      "Sign in to Friday Night Games",
    );
  });
  it("selects plural forms", () => {
    expect(t("format.minutes", { count: 1 })).toBe("1 minute");
    expect(t("format.minutes", { count: 5 })).toBe("5 minutes");
  });
});

describe("formatters", () => {
  it("formats sizes in binary units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(4 * 1024 ** 3)).toBe("4.0 GB");
    expect(formatBytes(-1)).toBe("—");
  });
  it("formats durations", () => {
    expect(formatDuration(42)).toBe("42 seconds");
    expect(formatDuration(180)).toBe("3 minutes");
    expect(formatDuration(4800)).toBe("1 h 20 min");
  });
});
