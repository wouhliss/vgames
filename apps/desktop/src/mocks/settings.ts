// Settings the Rust core keeps: account sessions, credential storage, download limits, social and
// overlay preferences, third-party notices. `persistKey` makes the settings survive a "restart" of the
// mock (a new backend on the same storage), which is how the settings tests check persistence.
import type {
  AccountSession,
  AppError,
  CredentialStorage,
  DownloadSettings,
  PackageOverlay,
  SettingsError,
  SocialSettings,
  SocialSettingsError,
} from "../ipc";
import { fail, type Handler } from "./runtime";

export interface SettingsState {
  /** Sessions per server id. */
  sessions: Record<string, AccountSession[]>;
  credentialStorage: CredentialStorage;
  downloadSettings: DownloadSettings;
  socialSettings: SocialSettings;
  overlayPackages: PackageOverlay[];
  /** Accelerators the "operating system" already uses (hotkey conflicts). */
  takenHotkeys: Record<string, string | null>;
  licenses: string;
  /** localStorage key the persisted settings are saved under; null = in memory only. */
  persistKey: string | null;
}

export function defaultSettingsState(): SettingsState {
  return {
    sessions: {},
    credentialStorage: { kind: "keychain" },
    downloadSettings: { bandwidth_limit_kib: null, concurrent_installs: 1 },
    socialSettings: {
      show_current_game: true,
      do_not_disturb: false,
      overlay_enabled: true,
      overlay_hotkey: "Shift+F3",
    },
    overlayPackages: [],
    takenHotkeys: { "Alt+Tab": "the operating system", "Ctrl+Alt+Delete": "the operating system" },
    licenses:
      "vgames launcher\n\nThird-party notices\n\nreact 19 — MIT License\nCopyright (c) Meta Platforms, Inc. and affiliates.\n\n@tanstack/react-query — MIT License\nCopyright (c) 2021-present Tanner Linsley",
    persistKey: null,
  };
}

/** Keys of the mock state that a restart keeps. */
export const PERSISTED = [
  "appearance",
  "servers",
  "libraries",
  "downloadSettings",
  "socialSettings",
  "overlayPackages",
  "sessions",
  "defaultRunner",
  "compatOverrides",
  "runtimes",
] as const;

export function loadPersisted(key: string | null): Record<string, unknown> {
  if (!key) return {};
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

export function savePersisted(key: string | null, state: Record<string, unknown>): void {
  if (!key) return;
  const out: Record<string, unknown> = {};
  for (const k of PERSISTED) out[k] = state[k];
  try {
    localStorage.setItem(key, JSON.stringify(out));
  } catch {
    // Storage may be unavailable; the mock keeps working in memory.
  }
}

export function settingsHandlers(
  state: SettingsState & { servers: { id: string }[] },
): Record<string, Handler> {
  return {
    account_sessions: (args) => {
      const serverId = String(args.serverId);
      if (!state.servers.some((s) => s.id === serverId))
        fail({ kind: "not_found" } satisfies AppError);
      return state.sessions[serverId] ?? [];
    },
    account_session_revoke: (args) => {
      const serverId = String(args.serverId);
      const list = state.sessions[serverId] ?? [];
      const target = list.find((s) => s.id === args.sessionId);
      if (!target) fail({ kind: "not_found" } satisfies AppError);
      if (target.current)
        fail({
          kind: "invalid_input",
          field: "sessionId",
          message: "sign out instead",
        } satisfies AppError);
      state.sessions = { ...state.sessions, [serverId]: list.filter((s) => s !== target) };
      return null;
    },
    credential_storage: () => state.credentialStorage,

    download_settings_get: () => state.downloadSettings,
    download_settings_set: (args) => {
      const next = args.settings as DownloadSettings;
      if (
        next.bandwidth_limit_kib !== null &&
        (!Number.isFinite(next.bandwidth_limit_kib) || next.bandwidth_limit_kib < 100)
      )
        fail({
          kind: "invalid",
          field: "bandwidth_limit_kib",
          detail: "at least 100 KiB/s",
        } satisfies SettingsError);
      if (![1, 2, 3].includes(next.concurrent_installs))
        fail({
          kind: "invalid",
          field: "concurrent_installs",
          detail: "1 to 3",
        } satisfies SettingsError);
      state.downloadSettings = next;
      return next;
    },

    social_settings_get: () => state.socialSettings,
    social_settings_set: (args) => {
      const next = args.settings as SocialSettings;
      if (next.overlay_hotkey !== state.socialSettings.overlay_hotkey) {
        if (!/^((Ctrl|Alt|Shift|Super)\+)*[A-Za-z0-9`\-=[\];',./]\w*$/.test(next.overlay_hotkey))
          fail({ kind: "invalid" } satisfies SocialSettingsError);
        if (next.overlay_hotkey in state.takenHotkeys)
          fail({
            kind: "in_use",
            by: state.takenHotkeys[next.overlay_hotkey] ?? null,
          } satisfies SocialSettingsError);
      }
      state.socialSettings = next;
      return next;
    },
    overlay_packages: () => state.overlayPackages,
    overlay_package_set: (args) => {
      const ref = args.package as { server_id: string; package_id: string };
      state.overlayPackages = state.overlayPackages.map((p) =>
        p.package.server_id === ref.server_id && p.package.package_id === ref.package_id
          ? {
              ...p,
              enabled: Boolean(args.enabled),
              disabled_by_safety_valve_at: args.enabled ? null : p.disabled_by_safety_valve_at,
            }
          : p,
      );
      return null;
    },
    app_licenses: () => state.licenses,
  };
}
