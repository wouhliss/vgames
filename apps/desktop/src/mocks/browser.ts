// Mock mode for the browser (`pnpm --filter @vgames/desktop dev:mock`) and Playwright.
//
// Pick a starting state with `?mock=<preset>` (kept for the session). Playwright drives Rust
// events through `window.__vgamesMock.emit(name, payload)`.
import { emit } from "@tauri-apps/api/event";
import {
  installMockBackend,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockBackend,
  type MockState,
} from "./backend";
import { makeCatalog } from "./catalog";
import { MOCK_COLLECTIONS, makeInstalls, OFFLINE_LIBRARY } from "./library";

function withInstalls(count: number): Partial<MockState> {
  const libraries = [MOCK_LIBRARY, OFFLINE_LIBRARY];
  return {
    servers: [MOCK_SERVER],
    libraries,
    collections: MOCK_COLLECTIONS,
    installs: makeInstalls(count, MOCK_SERVER, libraries),
    packages: makeCatalog(120),
  };
}

const PRESETS: Record<string, () => Partial<MockState>> = {
  /** First run: no server, no library. */
  fresh: () => ({}),
  /** Signed in, with a library holding a few dozen packages (one drive offline). */
  ready: () => withInstalls(40),
  /** Signed in with an empty library. */
  empty: () => ({ servers: [MOCK_SERVER], libraries: [MOCK_LIBRARY], packages: makeCatalog(120) }),
  /** 5,000 installed packages (performance). */
  huge: () => withInstalls(5000),
  /** Server added and signed in, but no library yet. */
  "no-library": () => ({ servers: [MOCK_SERVER] }),
  /** Debug build (http://localhost allowed). */
  debug: () => ({
    appInfo: { version: "0.4.0", profile: null, debug_build: true, os: "linux", arch: "x86_64" },
  }),
};

declare global {
  interface Window {
    __vgamesMock?: {
      backend: MockBackend;
      emit: (event: string, payload: unknown) => Promise<void>;
    };
  }
}

export function installBrowserMocks(): void {
  const params = new URLSearchParams(window.location.search);
  let preset = params.get("mock");
  try {
    if (preset) sessionStorage.setItem("vgames.mock", preset);
    else preset = sessionStorage.getItem("vgames.mock");
  } catch {
    // Storage may be unavailable; the URL parameter still works.
  }
  const overrides = (PRESETS[preset ?? "ready"] ?? PRESETS.ready)?.() ?? {};
  const settings = params.get("mockState");
  const extra = settings ? (JSON.parse(settings) as Partial<MockState>) : {};
  const backend = installMockBackend({ ...overrides, ...extra });
  window.__vgamesMock = { backend, emit: (event, payload) => emit(event, payload) };
}
