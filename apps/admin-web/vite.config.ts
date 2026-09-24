import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Served by vgames-api under /admin/ (same origin as the API: no CORS, cookie auth with
// SameSite=Lax + CSRF header). In dev, proxy API calls to the local API.
//
// `--mode mock` replaces the API with MSW (src/mocks). Its service worker is generated into
// node_modules/.vgames-mock-public (`pnpm mock:worker`) and served in that mode only, so production
// builds never contain it and no generated file is committed.
export default defineConfig(({ mode }) => ({
  base: "/admin/",
  plugins: [react()],
  publicDir: mode === "mock" ? "node_modules/.vgames-mock-public" : "public",
  server: {
    port: 5173,
    strictPort: true,
    proxy:
      mode === "mock"
        ? {}
        : { "/v1": "http://localhost:8080", "/.well-known": "http://localhost:8080" },
  },
  build: {
    target: "es2023",
    sourcemap: true,
  },
}));
