// `vite --mode mock`: the admin web in a browser against MSW. `?mock=owner|admin|user|anon` picks
// who is signed in (kept for the session). The service worker file lives in mock-public/, which is
// only served in mock mode, so production builds never ship it.
import { setupWorker } from "msw/browser";
import { createDb, type Role } from "./db";
import { createHandlers } from "./handlers";

export async function startMockWorker(): Promise<void> {
  const params = new URLSearchParams(window.location.search);
  let who = params.get("mock");
  try {
    if (who) sessionStorage.setItem("vgames.admin.mock", who);
    else who = sessionStorage.getItem("vgames.admin.mock");
  } catch {
    // Storage unavailable: the URL parameter still works.
  }
  const role: Role | null =
    who === "anon" ? null : who === "owner" || who === "user" ? who : "admin";
  const db = createDb(role);
  // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
  document.cookie = role ? `__Host-vgames_csrf=${db.csrf}; path=/; secure` : "";
  const worker = setupWorker(...createHandlers(db));
  await worker.start({
    serviceWorker: { url: "/admin/mockServiceWorker.js" },
    onUnhandledRequest: "bypass",
    quiet: true,
  });
}
