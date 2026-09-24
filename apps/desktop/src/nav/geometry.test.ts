import { describe, expect, it } from "vitest";
import { pickNext, type Rect } from "./geometry";

const box = (left: number, top: number, w = 100, h = 100): Rect => ({
  left,
  top,
  right: left + w,
  bottom: top + h,
});

describe("pickNext", () => {
  // 3×3 grid of 100px tiles with 20px gaps.
  const grid = [0, 1, 2].flatMap((row) => [0, 1, 2].map((col) => box(col * 120, row * 120)));
  const center = grid[4] as Rect;
  const others = grid.filter((r) => r !== center);

  it.each([
    ["up", 1],
    ["down", 7],
    ["left", 3],
    ["right", 5],
  ] as const)("moves %s to the adjacent tile", (dir, expected) => {
    const index = pickNext(center, others, dir);
    expect(others[index]).toBe(grid[expected]);
  });

  it("returns -1 when nothing lies in that direction", () => {
    expect(pickNext(grid[0] as Rect, grid.slice(1), "up")).toBe(-1);
    expect(pickNext(grid[0] as Rect, grid.slice(1), "left")).toBe(-1);
  });

  it("prefers an aligned item over a closer diagonal one", () => {
    const origin = box(0, 0, 200, 40); // a sidebar item
    const topBar = box(220, -60, 80, 30); // up-right, closer on the x axis
    const content = box(260, 0, 400, 300); // same row, further right
    expect(pickNext(origin, [topBar, content], "right")).toBe(1);
  });

  it("ignores empty (hidden) rects", () => {
    expect(pickNext(box(0, 0), [{ left: 0, top: 200, right: 0, bottom: 200 }], "down")).toBe(-1);
  });

  it("handles overlapping shapes by center position", () => {
    const tall = box(0, 0, 100, 300);
    const beside = box(120, 150, 100, 100);
    expect(pickNext(beside, [tall], "left")).toBe(0);
    expect(pickNext(tall, [beside], "down")).toBe(-1);
  });
});
