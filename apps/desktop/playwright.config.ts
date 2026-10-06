import { defineConfig, devices } from "@playwright/test";

// Launcher UI end-to-end tests. They run the real UI in Chromium in mock mode (`vite --mode mock`):
// the Rust core is replaced by the fixtures in src/mocks, driven per test through `?mock=` presets and
// `window.__vgamesMock`.
const PORT = 1421;
// A preinstalled Chromium can be used instead of Playwright's download (e.g. in sandboxes that
// cannot fetch browsers). CI leaves it unset.
const executablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE;

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: "retain-on-failure",
    ...(executablePath ? { launchOptions: { executablePath } } : {}),
  },
  projects: [
    {
      name: "chromium",
      testIgnore: /\.perf\.spec\.ts$/,
      // Reduced motion sets every transition to 0 ms (tokens.css), so accessibility scans see final
      // colours: a scan during a 150 ms dialog fade-in measured the caption's contrast mid-fade.
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1280, height: 800 },
        reducedMotion: "reduce",
      },
    },
    {
      // Frame-time measurements run alone, after every other test, so parallel workers (garbage
      // collection in the memory test, page loads) cannot steal CPU from the measured frames.
      name: "perf",
      testMatch: /\.perf\.spec\.ts$/,
      dependencies: ["chromium"],
      use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 800 } },
    },
  ],
  webServer: {
    // A prebuilt bundle: deterministic and fast under parallel workers (the dev server compiles on demand).
    command: `pnpm build:mock && pnpm exec vite preview --outDir node_modules/.vgames-mock-dist --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
