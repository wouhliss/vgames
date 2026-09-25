import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const root = (path: string) => fileURLToPath(new URL(path, import.meta.url));

// Pages: the main window, plus the overlay window (Agent 4, A4-T10: macOS panel and fallback
// window) once its entry module exists.
const input: Record<string, string> = { main: root("./index.html") };
if (existsSync(root("./src/overlay/main.tsx"))) input.overlay = root("./overlay.html");

// Tauri expects a fixed dev port and must see Rust-side errors in the terminal.
//
// Modes:
// - `development` / `production`: the real app, talking to the Rust core over IPC.
// - `mock`: the same app with `@tauri-apps/api/mocks` fixtures installed (src/mocks), so the UI can
//   run in a plain browser for development and Playwright. Mock code is only imported behind
//   `import.meta.env.MODE === "mock"`, so production bundles never contain it.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2023",
    sourcemap: false,
    // Keep the bundle small: the launcher must stay light (docs/architecture/00-overview.md).
    chunkSizeWarningLimit: 400,
    rolldownOptions: { input },
  },
});
