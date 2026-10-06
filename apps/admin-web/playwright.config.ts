import { defineConfig, devices } from "@playwright/test";

// Admin web E2E. By default against the MSW mock build (fast, CI). Set ADMIN_E2E_BASE_URL to run the
// same specs against the real stack (nightly e2e.yml).
const PORT = 5174;
const external = process.env.ADMIN_E2E_BASE_URL;
// A preinstalled Chromium can be used instead of Playwright's download. CI leaves it unset.
const executablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE;

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  // Each real worker signs in once; four OAuth flows fit the shared 20/minute auth budget.
  ...(external ? { workers: 4 } : {}),
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: external ?? `http://localhost:${PORT}`,
    // Real traces include authenticated session cookies; keep them out of artifacts.
    trace: external ? "off" : "retain-on-failure",
    ...(executablePath ? { launchOptions: { executablePath } } : {}),
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  ...(external
    ? {}
    : {
        webServer: {
          command: `pnpm build:mock && pnpm exec vite preview --mode mock --outDir node_modules/.vgames-mock-dist --port ${PORT} --strictPort`,
          url: `http://localhost:${PORT}/admin/`,
          reuseExistingServer: !process.env.CI,
          timeout: 120_000,
        },
      }),
});
