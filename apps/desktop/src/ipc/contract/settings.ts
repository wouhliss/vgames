// Settings that are not covered by another domain: the overlay's per-package switches, privacy and
// overlay preferences (Agent 4's `SocialSettings`, 05-social-notes §5) and third-party licenses
// (Agents 2 and 4). Account sessions and token storage are generated (INS-05).
// Requested shapes; see core.ts for the conventions.
import type { PackageOverlay, PackageRef, SocialError } from "../../bindings";
import type { AppError } from "./core";
import { call, type Result } from "./runtime";

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
