// `vite --mode mock`: the admin web in a browser against MSW. `?mock=owner|admin|user|anon` picks
// who is signed in (kept for the session). The service worker file lives in mock-public/, which is
// only served in mock mode, so production builds never ship it.
import { setupWorker } from "msw/browser";
import { createDb, type MockDb, type Role } from "./db";
import { createHandlers } from "./handlers";
import { editElsewhere } from "./packages";

declare global {
  interface Window {
    /** Playwright drives the mock server through this (mock mode only). */
    __adminMock?: { db: MockDb; editElsewhere: typeof editElsewhere };
  }
}

const DB_KEY = "vgames.admin.mockdb";

function loadDb(role: Role | null): MockDb | null {
  try {
    const raw = sessionStorage.getItem(DB_KEY);
    const db = raw ? (JSON.parse(raw) as MockDb) : null;
    return db && db.role === role ? db : null;
  } catch {
    return null;
  }
}

function saveDb(db: MockDb): void {
  try {
    sessionStorage.setItem(DB_KEY, JSON.stringify(db));
  } catch {
    // Storage full or unavailable: the mock keeps working in memory.
  }
}

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
  // The mock server's data survives reloads (like a real server) for the tab's lifetime; opening a
  // `?mock=` URL starts over.
  const db = (params.get("mock") ? null : loadDb(role)) ?? createDb(role);
  window.__adminMock = { db, editElsewhere };
  // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
  document.cookie = role ? `__Host-vgames_csrf=${db.csrf}; path=/; secure` : "";
  const worker = setupWorker(...createHandlers(db));
  worker.events.on("response:mocked", () => saveDb(db));
  await worker.start({
    serviceWorker: { url: "/admin/mockServiceWorker.js" },
    onUnhandledRequest: "bypass",
    quiet: true,
  });
}
