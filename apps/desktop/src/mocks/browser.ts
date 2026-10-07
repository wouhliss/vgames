// Mock mode for the browser (`pnpm --filter @vgames/desktop dev:mock`) and Playwright.
//
// Pick a starting state with `?mock=<preset>` (kept for the session). Playwright drives Rust
// events through `window.__vgamesMock.emit(name, payload)`.
import { emit } from "@tauri-apps/api/event";
import {
  installMockBackend,
  MOCK_ACCOUNT,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockBackend,
  type MockState,
} from "./backend";
import { makeCatalog } from "./catalog";
import { makeDownloads, startDownloadSimulation } from "./downloads";
import { MOCK_COLLECTIONS, makeInstalls, OFFLINE_LIBRARY } from "./library";
import { BEA, ME, makeInvite } from "./social";

function withInstalls(count: number, downloads = false): Partial<MockState> {
  const libraries = [MOCK_LIBRARY, OFFLINE_LIBRARY];
  const packages = makeCatalog(120);
  const installs = makeInstalls(count, MOCK_SERVER, libraries);
  return {
    servers: [MOCK_SERVER],
    libraries,
    collections: MOCK_COLLECTIONS,
    installs,
    packages,
    ...(downloads
      ? {
          ...makeDownloads(packages, installs, MOCK_SERVER.id, MOCK_LIBRARY.id, MOCK_LIBRARY.path),
          downloadRate: 48 * 1024 ** 2,
        }
      : {}),
    updater: {
      current_version: "0.4.0",
      state: { kind: "available", version: "0.9.1", date: "2026-09-20" },
      blocked: null,
    },
    updateInstallMs: 4000,
    publishTickMs: 400,
    socialDelayMs: 400,
    // Bea is installing a game Sam invited her to.
    invites: [
      makeInvite({
        id: "01920000-0000-7000-8000-00000000e0b1",
        direction: "outgoing",
        from: ME,
        to: BEA,
        package: {
          id: installs[0]?.package.package_id ?? "",
          title: installs[0]?.title ?? "",
          cover_url: null,
        },
        state: "installing",
        progress: 0.42,
        message: null,
      }),
    ],
  };
}

const PRESETS: Record<string, () => Partial<MockState>> = {
  /** First run: no server, no library. */
  fresh: () => ({}),
  /** Signed in, with a library holding a few dozen packages (one drive offline). */
  ready: () => withInstalls(40, true),
  /** `ready`, signed in as an admin (publishing). */
  admin: () => {
    const ready = withInstalls(40, true);
    return {
      ...ready,
      servers: [{ ...MOCK_SERVER, account: { ...MOCK_ACCOUNT, role: "admin" } }],
    };
  },
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
  startDownloadSimulation(backend.state);
  window.__vgamesMock = { backend, emit: (event, payload) => emit(event, payload) };
}
