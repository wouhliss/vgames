// A3-T04 acceptance: 5,000 installed packages scroll without dropped frames. Runs in the "perf"
// project, alone and after every other test (playwright.config.ts), so the frames measured here
// share the CPU with nothing else from the suite.
import { expect, test } from "@playwright/test";
import { open } from "./helpers";

test("5,000 installed packages scroll without dropped frames", async ({ page }) => {
  test.setTimeout(90_000);
  await open(page, "huge");
  await expect(page.getByRole("tab", { name: "All 5000" })).toBeVisible();
  const domTiles = await page.getByRole("article").count();
  expect(domTiles).toBeLessThan(150);

  // Scroll through the whole list at ~6,000 px/s, one step per frame, and record frame times
  // and long tasks (anything that blocks the main thread for more than 50 ms).
  const run = await page.evaluate(async () => {
    const main = document.getElementById("main-content");
    if (!main) throw new Error("no main");
    const longTasks: number[] = [];
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) longTasks.push(entry.duration);
    });
    observer.observe({ type: "longtask" });
    const frames: number[] = [];
    await new Promise<void>((resolve) => {
      let last = performance.now();
      const end = last + 4000;
      const step = (now: number) => {
        frames.push(now - last);
        last = now;
        main.scrollTop += 100;
        if (now < end && main.scrollTop + main.clientHeight < main.scrollHeight) {
          requestAnimationFrame(step);
        } else resolve();
      };
      requestAnimationFrame(step);
    });
    observer.disconnect();
    return { frames: frames.slice(1), longTasks, scrolled: main.scrollTop };
  });
  const sorted = [...run.frames].sort((a, b) => a - b);
  const p95 = sorted[Math.floor(sorted.length * 0.95)] ?? 0;
  // A dropped frame is one that took longer than two refresh intervals (> 33 ms at 60 Hz).
  const dropped = run.frames.filter((f) => f > 34).length;
  const summary = `frames ${run.frames.length}, p95 ${p95.toFixed(1)} ms, max ${(sorted.at(-1) ?? 0).toFixed(1)} ms, dropped ${dropped}, long tasks ${run.longTasks.length}, scrolled ${run.scrolled} px, tiles in DOM ${domTiles}`;
  test.info().annotations.push({ type: "performance", description: summary });
  console.log(summary);
  expect(run.scrolled).toBeGreaterThan(20_000);
  expect(run.longTasks).toEqual([]);
  expect(dropped).toBe(0);
  expect(await page.getByRole("article").count()).toBeLessThan(150);
});
