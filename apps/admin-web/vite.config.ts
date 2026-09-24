import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Served by vgames-api under /admin/ (same origin as the API: no CORS, cookie
// auth with SameSite=Lax + CSRF header). In dev, proxy API calls to the local API.
export default defineConfig({
  base: "/admin/",
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      "/v1": "http://localhost:8080",
      "/.well-known": "http://localhost:8080",
    },
  },
  build: {
    target: "es2023",
    sourcemap: true,
  },
});
