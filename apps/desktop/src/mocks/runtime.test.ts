import { afterEach, describe, expect, it, vi } from "vitest";
import { cancelScheduled, later, schedule } from "./runtime";

describe("mock timers", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("cancelScheduled stops pending work so it never runs after a test", () => {
    vi.useFakeTimers();
    const fired: string[] = [];
    later(2400, () => fired.push("install finished"));
    schedule(() => fired.push("message sent"), 300);
    cancelScheduled();
    vi.advanceTimersByTime(10_000);
    expect(fired).toEqual([]);
  });

  it("runs scheduled work normally and keeps clearTimeout working", () => {
    vi.useFakeTimers();
    const fired: string[] = [];
    later(0, () => fired.push("now"));
    later(100, () => fired.push("later"));
    clearTimeout(schedule(() => fired.push("cleared"), 50));
    vi.advanceTimersByTime(200);
    expect(fired).toEqual(["now", "later"]);
    // A timer that already ran is forgotten: cancelling afterwards is harmless.
    cancelScheduled();
    expect(fired).toEqual(["now", "later"]);
  });
});
