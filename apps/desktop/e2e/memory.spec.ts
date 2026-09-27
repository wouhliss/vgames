// A3-T02 acceptance: navigating every route 100 times shows no heap growth. Measured through the
// Chrome DevTools Protocol after forced garbage collection; also checks DOM nodes and event
// listeners, which catch leaked subscriptions (e.g. Tauri event listeners not removed on unmount).
import { type CDPSession, expect, type Page, test } from "@playwright/test";
import { open } from "./helpers";

const ROUTES = ["Browse", "Friends", "Downloads", "Settings", "Library"];

async function measure(cdp: CDPSession) {
  for (let i = 0; i < 3; i += 1) await cdp.send("HeapProfiler.collectGarbage");
  const { usedSize } = await cdp.send("Runtime.getHeapUsage");
  const { metrics } = await cdp.send("Performance.getMetrics");
  const metric = (name: string) => metrics.find((m) => m.name === name)?.value ?? 0;
  return { heap: usedSize, nodes: metric("Nodes"), listeners: metric("JSEventListeners") };
}

async function cycle(page: Page, times: number) {
  const nav = page.getByRole("navigation", { name: "Main" });
  for (let i = 0; i < times; i += 1) {
    for (const name of ROUTES) {
      await nav.getByRole("link", { name }).click();
      await expect(page.getByRole("heading", { level: 1, name })).toBeVisible();
    }
  }
}

test("navigating all routes 100 times does not grow memory (steady state)", async ({ page }) => {
  test.setTimeout(240_000);
  // The simulated download is paused: its 4 progress events per second re-render the top bar, and a
  // tick landing between the forced GC and the reading shows up as one extra listener. This test is
  // about what navigation leaves behind, so it measures an otherwise idle app.
  await open(page, "ready", { downloadRate: 0 });
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");

  await cycle(page, 10); // warm up: lazy chunks, query cache, JIT
  const before = await measure(cdp);
  await cycle(page, 100);
  const mid = await measure(cdp);
  await cycle(page, 100);
  const after = await measure(cdp);

  // The first 100 cycles still settle (JIT, router and query caches: ~0.8 MB once). A leak shows
  // as growth that continues, so the assertion is on the second 100-cycle window.
  const growth = after.heap - mid.heap;
  test.info().annotations.push({
    type: "memory",
    description: `heap ${(before.heap / 1e6).toFixed(2)} → ${(mid.heap / 1e6).toFixed(2)} → ${(after.heap / 1e6).toFixed(2)} MB (steady window ${(growth / 1e3).toFixed(0)} KB); nodes ${before.nodes} → ${after.nodes}; listeners ${before.listeners} → ${after.listeners}`,
  });
  console.log(test.info().annotations.at(-1)?.description);
  expect(after.listeners).toBeLessThanOrEqual(before.listeners);
  expect(after.nodes).toBeLessThanOrEqual(before.nodes * 1.05);
  expect(after.listeners).toBeLessThanOrEqual(mid.listeners);
  expect(growth).toBeLessThan(256 * 1024);
});
