// Settings that are not covered by another domain: third-party licenses (Agents 2 and 4).
// Account sessions, token storage, social and overlay settings (`social_settings_get|set`,
// `package_overlays_list|set`, hotkey errors as `SocialError`) are generated.
// Requested shapes; see core.ts for the conventions.
import type { AppError } from "./core";
import { call, type Result } from "./runtime";

export const settingsCommands = {
  /** Third-party notices bundled with the launcher (plain text). */
  async appLicenses(): Promise<Result<string, AppError>> {
    return call("app_licenses");
  },
};
