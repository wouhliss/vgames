// A3-T04 acceptance: 5,000 installed packages scroll without dropped frames caused by the page.
// Runs in the "perf" project, alone and after every other test (playwright.config.ts), so the frames
// measured here share the CPU with nothing else from the suite.
import { expect, test } from "@playwright/test";
import { axeScan, open } from "./helpers";

test("5,000 installed packages scroll without dropped frames", async ({ page }) => {
  test.setTimeout(90_000);
  await open(page, "huge");
  await expect(page.getByRole("tab", { name: "All 5000" })).toBeVisible();
  await axeScan(page);
  const domTiles = await page.getByRole("article").count();
  expect(domTiles).toBeLessThan(150);

  // Warm-up: one quick pass down and back, so the measurement below sees steady-state scrolling, not the
  // first compilation of the rendering code.
  await page.evaluate(async () => {
    const main = document.getElementById("main-content");
    if (!main) throw new Error("no main");
    for (let y = 0; y <= 6000; y += 600) {
      main.scrollTop = y;
      await new Promise((r) => requestAnimationFrame(r));
    }
    main.scrollTop = 0;
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  });

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
  // What the page controls must be perfect: no main-thread task over 50 ms (rendering too many tiles
  // or doing layout work per frame shows up here first). Frame pacing itself also depends on the
  // machine: shared CI VMs deschedule the browser now and then (a lone 50 ms frame with no long task
  // behind it) or render at 30 Hz for a while, so the pacing budget is 95 % of frames within two
  // refresh intervals and at most one late frame per 120.
  const allowedDropped = Math.floor(run.frames.length / 120);
  const summary = `frames ${run.frames.length}, p95 ${p95.toFixed(1)} ms, max ${(sorted.at(-1) ?? 0).toFixed(1)} ms, dropped ${dropped}, long tasks ${run.longTasks.length}, scrolled ${run.scrolled} px, tiles in DOM ${domTiles}`;
  test.info().annotations.push({ type: "performance", description: summary });
  console.log(summary);
  expect(run.scrolled).toBeGreaterThan(20_000);
  expect(run.longTasks).toEqual([]);
  expect(p95).toBeLessThanOrEqual(34);
  expect(dropped).toBeLessThanOrEqual(allowedDropped);
  expect(await page.getByRole("article").count()).toBeLessThan(150);
});
