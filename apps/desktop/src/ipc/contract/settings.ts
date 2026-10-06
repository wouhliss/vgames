// Settings that are not covered by another domain: account sessions and credential storage, library
// management, download limits, the overlay's per-package switches, privacy and overlay preferences
// (Agent 4's `SocialSettings`, 05-social-notes §5) and third-party licenses (Agents 2 and 4).
// Requested shapes; see core.ts for the conventions.
import type { PackageOverlay, PackageRef, SocialError } from "../../bindings";
import type { AppError } from "./core";
import { call, get, type Result } from "./runtime";

export type SessionPlatform = "windows" | "linux" | "macos" | "web" | "cli";

/** A signed-in device of this account on this server (`GET /v1/me/sessions`). */
export type AccountSession = {
  id: string;
  /** User agent summary the server stored, e.g. "vgames 0.4 on Windows". */
  device_name: string;
  platform: SessionPlatform;
  created_at: string;
  last_used_at: string | null;
  /** This launcher's own session. */
  current: boolean;
};

/**
 * Where the launcher keeps refresh tokens (01-security §7). `file_fallback`: no OS keychain was
 * available, so tokens sit in a file readable by this user; the UI warns about it.
 */
export type CredentialStorage = { kind: "keychain" } | { kind: "file_fallback"; path: string };

export type DownloadSettings = {
  /** Kibibytes per second; null = unlimited (02-package-format §7.12). */
  bandwidth_limit_kib: number | null;
  /** How many installs run at the same time, 1–3. */
  concurrent_installs: number;
};

export type SettingsError =
  | { kind: "invalid"; field: string; detail: string }
  | { kind: "io"; detail: string };

// `social_settings_get|set` and `SocialSettings` (05-social-notes §5) are generated now; the typed
// hotkey errors below are still requested as `SocialError` variants (A4-T10 registers the hotkey).

export type HotkeyError =
  /** Not a valid accelerator (no key, or a modifier alone). */
  | { kind: "invalid" }
  /** The operating system or another application already uses it. */
  | { kind: "in_use"; by: string | null };

/** What saving social settings can report: the generated errors plus the requested hotkey ones. */
export type SocialSettingsError = SocialError | HotkeyError;

export const settingsCommands = {
  async accountSessions(serverId: string): Promise<Result<AccountSession[], AppError>> {
    return call("account_sessions", { serverId });
  },
  async accountSessionRevoke(serverId: string, sessionId: string): Promise<Result<null, AppError>> {
    return call("account_session_revoke", { serverId, sessionId });
  },
  async credentialStorage(): Promise<CredentialStorage> {
    return get("credential_storage");
  },

  async downloadSettingsGet(): Promise<DownloadSettings> {
    return get("download_settings_get");
  },
  async downloadSettingsSet(
    settings: DownloadSettings,
  ): Promise<Result<DownloadSettings, SettingsError>> {
    return call("download_settings_set", { settings });
  },

  async overlayPackages(): Promise<Result<PackageOverlay[], AppError>> {
    // Generated as `packageOverlaysList` (main-window commands never start with `overlay_`).
    return call("package_overlays_list");
  },
  /** Turning it on also clears the safety valve. */
  async overlayPackageSet(pkg: PackageRef, enabled: boolean): Promise<Result<null, AppError>> {
    return call("package_overlay_set", { packageRef: pkg, enabled });
  },

  /** Third-party notices bundled with the launcher (plain text). */
  async appLicenses(): Promise<Result<string, AppError>> {
    return call("app_licenses");
  },
};
