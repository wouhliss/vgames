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
  test.setTimeout(420_000);
  // The simulated download is paused: its 4 progress events per second re-render the top bar, and a
  // tick landing between the forced GC and the reading shows up as one extra listener. This test is
  // about what navigation leaves behind, so it measures an otherwise idle app.
  await open(page, "ready", { downloadRate: 0 });
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");

  await cycle(page, 10); // warm up: lazy chunks, query cache, JIT
  const before = await measure(cdp);
  // The first 100 cycles still settle (JIT, router and query caches: ~1 MB once).
  await cycle(page, 100);
  let previous = await measure(cdp);
  // Then three more windows of 100. V8 grows the heap in steps, so one window alone lands anywhere
  // between ~30 and ~260 KB on an idle app (measured: 212/146/28/250 KB in one run). A leak grows
  // every window, so the smallest window is the one held to the budget: ≥ 2.6 KB leaked per round
  // still fails. Listeners and DOM nodes must stay flat after every window.
  const windows: number[] = [];
  for (let w = 0; w < 3; w += 1) {
    await cycle(page, 100);
    const now = await measure(cdp);
    windows.push(now.heap - previous.heap);
    expect(now.listeners, `listeners after window ${w + 1}`).toBeLessThanOrEqual(before.listeners);
    expect(now.nodes, `DOM nodes after window ${w + 1}`).toBeLessThanOrEqual(before.nodes * 1.05);
    previous = now;
  }
  const smallest = Math.min(...windows);
  test.info().annotations.push({
    type: "memory",
    description: `heap ${(before.heap / 1e6).toFixed(2)} → ${(previous.heap / 1e6).toFixed(2)} MB; windows ${windows.map((w) => `${(w / 1e3).toFixed(0)} KB`).join(" / ")}; nodes ${before.nodes} → ${previous.nodes}; listeners ${before.listeners} → ${previous.listeners}`,
  });
  console.log(test.info().annotations.at(-1)?.description);
  expect(smallest).toBeLessThan(256 * 1024);
});
