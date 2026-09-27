// In-memory stand-in for the Rust core, built on `@tauri-apps/api/mocks`. Used by unit tests, by
// `vite --mode mock` (the UI in a plain browser) and by the Playwright suite.
//
// Payloads must match `src/ipc` types exactly: every fixture is typed against them, so a contract
// change breaks the mocks at compile time.
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type {
  Account,
  AppError,
  AppearanceSettings,
  AppInfo,
  AuthError,
  AuthFlow,
  FolderPick,
  Library,
  LibraryError,
  LibraryRemoveError,
  ServerError,
  ServerPreview,
  ServerProfile,
  UpdateCheck,
  UpdaterStatus,
} from "../ipc";
import { events } from "../ipc";
import { type CatalogState, catalogHandlers, defaultCatalogState } from "./catalog";
import { type DownloadsState, defaultDownloadsState, downloadHandlers } from "./downloads";
import { defaultLibraryState, type LibraryState, libraryHandlers } from "./library";
import { CommandFailure, fail, type Handler } from "./runtime";
import {
  defaultSettingsState,
  loadPersisted,
  type SettingsState,
  savePersisted,
  settingsHandlers,
} from "./settings";

export { fail, type Handler } from "./runtime";

export interface MockCall {
  cmd: string;
  args: Record<string, unknown>;
}

export const FINGERPRINT = "VG1-7K2M-Q9XD-4HT8-B3NW-RC5E-X1JP-V6GA-M0ZF";
export const OTHER_FINGERPRINT = "VG1-J3KD-8PQX-NW2A-TY4B-C9RM-5EHV-G7ZS-1FQ0";

export const MOCK_ACCOUNT: Account = {
  user_id: "01920000-0000-7000-8000-00000000a001",
  username: "sam",
  display_name: "Sam",
  role: "user",
};

export const MOCK_SERVER: ServerProfile = {
  id: "01920000-0000-7000-8000-000000000001",
  url: "https://games.example.org",
  name: "Friday Night Games",
  fingerprint: FINGERPRINT,
  active: true,
  account: MOCK_ACCOUNT,
  last_connected_at: "2026-09-24T10:00:00Z",
};

export const MOCK_LIBRARY: Library = {
  id: "01920000-0000-7000-8000-0000000000b1",
  path: "/home/sam/Games",
  label: null,
  is_default: true,
  online: true,
  free_bytes: 412 * 1024 ** 3,
  total_bytes: 931 * 1024 ** 3,
  install_count: 0,
};

export interface MockState extends LibraryState, CatalogState, DownloadsState, SettingsState {
  appInfo: AppInfo;
  appearance: AppearanceSettings;
  servers: ServerProfile[];
  libraries: Library[];
  /** Result of the native folder picker: a folder, `null` (cancelled) or an error. */
  folderPick: FolderPick | null | { error: AppError };
  /** Error for `library_add`, if any. */
  libraryAddError: LibraryError | null;
  /** How long sign-in takes in the browser before `auth-finished` arrives (ms). */
  authDelayMs: number;
  diagnostics: string;
  /** What `updater_check` returns (a check the user asked for). */
  updateCheck: UpdateCheck;
  /** What `updater_status` returns. */
  updater: UpdaterStatus;
}

export function defaultState(): MockState {
  return {
    ...defaultLibraryState(),
    ...defaultCatalogState(),
    ...defaultDownloadsState(),
    ...defaultSettingsState(),
    appInfo: {
      version: "0.4.0",
      profile: null,
      debug_build: false,
      os: "linux",
      arch: "x86_64",
    },
    appearance: { theme: "dark", reduce_motion: false },
    servers: [],
    libraries: [],
    folderPick: {
      path: "/home/sam/Games",
      free_bytes: 412 * 1024 ** 3,
      total_bytes: 931 * 1024 ** 3,
    },
    libraryAddError: null,
    authDelayMs: 600,
    updateCheck: { kind: "available", version: "0.9.1" },
    updater: {
      current_version: "0.4.0",
      state: { kind: "available", version: "0.9.1", date: "2026-09-20" },
      blocked: null,
    },
    diagnostics: "vgames 0.4.0 (linux x86_64)\nservers: 1\nlibraries: 1",
  };
}

// ------------------------------------------------------------------------------------------------
// Server scenarios, chosen by host name so one fixture set covers every onboarding edge case.

interface HostBehaviour {
  preview?: ServerError;
  name?: string;
  fingerprint?: string;
  registration?: ServerPreview["registration_mode"];
  /** Sign-in result: an error, "paste" (no deep link arrives; the code fallback works) or success. */
  auth?: AuthError | "paste";
}

export const HOSTS: Record<string, HostBehaviour> = {
  "games.example.org": { name: "Friday Night Games", registration: "allowlist" },
  "unreachable.example": { preview: { kind: "unreachable", detail: "connection refused" } },
  "slow.example": { preview: { kind: "timeout" } },
  "bad-cert.example": { preview: { kind: "tls", detail: "certificate has expired" } },
  "not-vgames.example": { preview: { kind: "not_vgames" } },
  "future.example": {
    preview: { kind: "launcher_too_old", min_version: "0.9.0", current_version: "0.4.0" },
  },
  "closed.example": {
    name: "Closed Club",
    registration: "closed",
    auth: { kind: "registration_closed" },
  },
  "invite-only.example": {
    name: "Invite Only",
    registration: "allowlist",
    auth: { kind: "not_allowlisted" },
  },
  "disabled.example": {
    name: "Strict Server",
    registration: "open",
    auth: { kind: "user_disabled" },
  },
  "paste.example": { name: "No Deep Links", registration: "open", auth: "paste" },
  "nobrowser.example": {
    name: "Headless",
    registration: "open",
    auth: { kind: "browser_unavailable" },
  },
};

/** Mirrors the Rust normalization: https added when missing; http only for localhost in debug builds. */
function normalizeUrl(raw: string, debug: boolean): URL | ServerError {
  const trimmed = raw.trim();
  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return { kind: "invalid_url" };
  }
  if (
    !url.hostname ||
    /\s/.test(trimmed) ||
    (!url.hostname.includes(".") && url.hostname !== "localhost")
  ) {
    return { kind: "invalid_url" };
  }
  if (url.protocol === "http:") {
    if (debug && (url.hostname === "localhost" || url.hostname === "127.0.0.1")) return url;
    return { kind: "insecure_scheme" };
  }
  if (url.protocol !== "https:") return { kind: "invalid_url" };
  return url;
}

// ------------------------------------------------------------------------------------------------

export interface MockBackend {
  state: MockState;
  calls: MockCall[];
  /** Overrides or adds a command handler. */
  on: (cmd: string, handler: Handler) => void;
  callsTo: (cmd: string) => MockCall[];
}

let previewSeq = 0;

export function installMockBackend(overrides: Partial<MockState> = {}): MockBackend {
  const initial: MockState = { ...defaultState(), ...overrides };
  // A "restart": what the previous backend saved under the same key wins over the starting state.
  const state: MockState = { ...initial, ...loadPersisted(initial.persistKey) };
  const calls: MockCall[] = [];
  const previews = new Map<string, { preview: ServerPreview; host: string }>();
  const flows = new Map<
    string,
    { serverId: string; host: string; timer?: ReturnType<typeof setTimeout> }
  >();

  const hostOf = (serverId: string) => {
    const server = state.servers.find((s) => s.id === serverId);
    return server ? new URL(server.url).hostname : "";
  };

  const handlers: Record<string, Handler> = {
    app_ready: () => null,
    app_info: () => state.appInfo,
    app_diagnostics: () => state.diagnostics,
    open_external_url: (args) => {
      const url = String(args.url);
      if (!/^https?:\/\//.test(url))
        fail({ kind: "invalid_input", field: "url", message: "only http(s)" } satisfies AppError);
      return null;
    },
    appearance_get: () => state.appearance,
    appearance_set: (args) => {
      state.appearance = args.settings as AppearanceSettings;
      return state.appearance;
    },

    servers_list: () => state.servers,
    server_preview: (args) => {
      const normalized = normalizeUrl(String(args.url), state.appInfo.debug_build);
      if (!(normalized instanceof URL)) fail(normalized);
      const host = normalized.hostname;
      const behaviour = HOSTS[host] ?? { name: host };
      if (behaviour.preview) fail(behaviour.preview);
      const fingerprint = behaviour.fingerprint ?? FINGERPRINT;
      const expected = (args.expectedFingerprint as string | null) ?? null;
      if (expected !== null && expected !== fingerprint) {
        fail({ kind: "fingerprint_mismatch", expected, actual: fingerprint } satisfies ServerError);
      }
      const existing = state.servers.find((s) => new URL(s.url).hostname === host);
      if (existing) fail({ kind: "already_added", server_id: existing.id } satisfies ServerError);
      previewSeq += 1;
      const preview: ServerPreview = {
        preview_id: `preview-${previewSeq}`,
        url: normalized.origin,
        server_id: `01920000-0000-7000-8000-${String(previewSeq).padStart(12, "0")}`,
        name: behaviour.name ?? host,
        motd: null,
        fingerprint,
        registration_mode: behaviour.registration ?? "open",
        expected_fingerprint: expected,
      };
      previews.set(preview.preview_id, { preview, host });
      return preview;
    },
    server_confirm: (args) => {
      const entry = previews.get(String(args.previewId));
      if (!entry) fail({ kind: "preview_expired" } satisfies ServerError);
      const profile: ServerProfile = {
        id: entry.preview.server_id,
        url: entry.preview.url,
        name: entry.preview.name,
        fingerprint: entry.preview.fingerprint,
        active: true,
        account: null,
        last_connected_at: null,
      };
      state.servers = [...state.servers.map((s) => ({ ...s, active: false })), profile];
      void events.serversChanged.emit({});
      return profile;
    },
    server_switch: (args) => {
      const target = state.servers.find((s) => s.id === args.serverId);
      if (!target) fail({ kind: "not_found" } satisfies AppError);
      state.servers = state.servers.map((s) => ({ ...s, active: s.id === target.id }));
      void events.serversChanged.emit({});
      return { ...target, active: true };
    },
    server_remove: (args) => {
      state.servers = state.servers.filter((s) => s.id !== args.serverId);
      void events.serversChanged.emit({});
      return null;
    },

    auth_start: (args) => {
      const serverId = String(args.serverId);
      const host = hostOf(serverId);
      const behaviour = HOSTS[host];
      const browserOpened = !(
        behaviour?.auth &&
        behaviour.auth !== "paste" &&
        behaviour.auth.kind === "browser_unavailable"
      );
      const flow: AuthFlow = {
        flow_id: `flow-${Date.now()}-${flows.size}`,
        expires_at: new Date(Date.now() + 600_000).toISOString(),
        browser_opened: browserOpened,
      };
      const entry: { serverId: string; host: string; timer?: ReturnType<typeof setTimeout> } = {
        serverId,
        host,
      };
      flows.set(flow.flow_id, entry);
      if (behaviour?.auth !== "paste" && browserOpened) {
        entry.timer = setTimeout(() => {
          if (!flows.has(flow.flow_id)) return;
          flows.delete(flow.flow_id);
          const error = behaviour?.auth;
          if (error && error !== "paste") {
            void events.authFinished.emit({
              flow_id: flow.flow_id,
              outcome: { kind: "failed", error },
            });
          } else {
            signIn(serverId);
            void events.authFinished.emit({
              flow_id: flow.flow_id,
              outcome: { kind: "signed_in", account: MOCK_ACCOUNT },
            });
          }
        }, state.authDelayMs);
      }
      return flow;
    },
    auth_open_browser: (args) => {
      const flow = flows.get(String(args.flowId));
      if (!flow) fail({ kind: "expired" } satisfies AuthError);
      const behaviour = HOSTS[flow.host];
      if (
        behaviour?.auth &&
        behaviour.auth !== "paste" &&
        behaviour.auth.kind === "browser_unavailable"
      )
        fail(behaviour.auth);
      return null;
    },
    auth_submit_code: (args) => {
      const flow = flows.get(String(args.flowId));
      if (!flow) fail({ kind: "expired" } satisfies AuthError);
      const code = String(args.code).trim();
      if (code !== "VALID-CODE-0123456789-abcdefghijklmnopqrstuvwxyz")
        fail({ kind: "invalid_code" } satisfies AuthError);
      clearTimeout(flow.timer);
      flows.delete(String(args.flowId));
      signIn(flow.serverId);
      return MOCK_ACCOUNT;
    },
    auth_cancel: (args) => {
      const flow = flows.get(String(args.flowId));
      clearTimeout(flow?.timer);
      flows.delete(String(args.flowId));
      return null;
    },

    auth_sign_out: (args) => {
      state.servers = state.servers.map((s) =>
        s.id === args.serverId ? { ...s, account: null } : s,
      );
      void events.serversChanged.emit({});
      return null;
    },

    updater_status: () => state.updater,
    updater_check: () => state.updateCheck,

    libraries_list: () => state.libraries,
    library_pick_folder: () => {
      const pick = state.folderPick;
      if (pick && "error" in pick) fail(pick.error);
      return pick;
    },
    library_add: (args) => {
      if (state.libraryAddError) fail(state.libraryAddError);
      const path = String(args.path);
      const pick = state.folderPick && !("error" in state.folderPick) ? state.folderPick : null;
      const library: Library = {
        ...MOCK_LIBRARY,
        id: `01920000-0000-7000-8000-0000000000${String(state.libraries.length + 1).padStart(2, "0")}`,
        path,
        is_default: Boolean(args.makeDefault) || state.libraries.length === 0,
        free_bytes: pick?.free_bytes ?? MOCK_LIBRARY.free_bytes,
        total_bytes: pick?.total_bytes ?? MOCK_LIBRARY.total_bytes,
      };
      state.libraries = [
        ...state.libraries.map((l) => (library.is_default ? { ...l, is_default: false } : l)),
        library,
      ];
      void events.librariesChanged.emit({});
      return library;
    },

    library_set_default: (args) => {
      const target = state.libraries.find((l) => l.id === args.libraryId);
      if (!target) fail({ kind: "not_found" } satisfies LibraryError);
      state.libraries = state.libraries.map((l) => ({ ...l, is_default: l.id === target.id }));
      void events.librariesChanged.emit({});
      return null;
    },
    library_remove: (args) => {
      const target = state.libraries.find((l) => l.id === args.libraryId);
      if (!target) fail({ kind: "not_found" } satisfies LibraryError);
      const count = state.installs.filter((i) => i.library_id === target.id).length;
      if (count > 0) fail({ kind: "not_empty", install_count: count } satisfies LibraryRemoveError);
      if (target.is_default) fail({ kind: "is_default" } satisfies LibraryRemoveError);
      state.libraries = state.libraries.filter((l) => l !== target);
      void events.librariesChanged.emit({});
      return null;
    },

    ...libraryHandlers(state),
    ...catalogHandlers(state),
    ...downloadHandlers(state),
    ...settingsHandlers(state),
  };

  function signIn(serverId: string) {
    state.servers = state.servers.map((s) =>
      s.id === serverId ? { ...s, account: MOCK_ACCOUNT } : s,
    );
    void events.serversChanged.emit({});
  }

  mockWindows("main");
  mockIPC(
    async (cmd, payload) => {
      const args = (payload ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args });
      const handler = handlers[cmd];
      if (!handler) throw new Error(`mock backend: no handler for command "${cmd}"`);
      try {
        const result = await handler(args);
        savePersisted(state.persistKey, state as unknown as Record<string, unknown>);
        return result;
      } catch (e) {
        // Tauri rejects with the serialized Rust error value (not an Error instance).
        if (e instanceof CommandFailure) return Promise.reject(e.error);
        throw e;
      }
    },
    { shouldMockEvents: true },
  );

  return {
    state,
    calls,
    on: (cmd, handler) => {
      handlers[cmd] = handler;
    },
    callsTo: (cmd) => calls.filter((c) => c.cmd === cmd),
  };
}

export function uninstallMockBackend(): void {
  clearMocks();
}
