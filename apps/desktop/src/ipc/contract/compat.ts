// Compatibility runtimes and a player's local overrides (Agent 2, A2-T16/T17; 09-compatibility §2–§5).
// Requested shapes; see core.ts for the conventions. The Rust core downloads and verifies runtimes
// from the signed runtime catalog and validates overrides (`vgames-core::compat`); the UI only shows
// and edits them. Installing Rosetta 2 is `rosetta_install` in catalog.ts.
import type { PackageRef } from "../../bindings";
import type { AppError } from "./core";
import { call, type Result } from "./runtime";

/** Runtime ids of the runtime catalog (`runtimes/catalog.toml`). */
export type RuntimeId =
  | "umu-proton"
  | "ge-proton"
  | "umu-launcher"
  | "wine-macos"
  | "d3dmetal"
  | "dxmt"
  | "dxvk-macos"
  | "moltenvk";

/** A Proton (Linux) or Wine (macOS) build from the runtime catalog. */
export type RunnerVersion = { runtime: RuntimeId; version: string };

/** macOS graphics backends, in the default order (09-compatibility §3). */
export type GraphicsBackend = "d3dmetal" | "dxmt" | "dxvk" | "wined3d";

/** How Windows games run on this computer. */
export type CompatHost =
  /** Windows: Windows games run natively; there is nothing to set up. */
  | { kind: "native" }
  | { kind: "proton" }
  | {
      kind: "wine";
      apple_silicon: boolean;
      /** Null on Intel Macs, which don't need it. */
      rosetta: "installed" | "missing" | null;
      /**
       * Last macOS release with full Rosetta 2, from the runtime catalog (e.g. "27"); null when
       * Apple has announced no end. The UI says games through Rosetta may stop working after it.
       */
      rosetta_last_macos: string | null;
    };

/** A runtime version on disk under `<app data>/runtimes/`. */
export type InstalledRuntime = {
  runtime: RuntimeId;
  version: string;
  size_bytes: number;
  /** Installed packages that launch with it. The core keeps it while this is above 0. */
  used_by: number;
};

export type CompatOverview = {
  host: CompatHost;
  runtimes: InstalledRuntime[];
  /** Runner versions from the catalog that run here, newest first (installed or downloadable). */
  runners: RunnerVersion[];
  /** Backends this Mac can use (D3DMetal only on Apple silicon); empty elsewhere. */
  graphics: GraphicsBackend[];
  /** The runner every package uses unless it overrides it; null = automatic (the compat profile's choice). */
  default_runner: RunnerVersion | null;
};

/** A player's local override for one package; stored on this computer, shown as such in diagnostics. */
export type CompatOverride = {
  /** Null = the default runner. */
  runner: RunnerVersion | null;
  /** macOS only; null = the compat profile's order. */
  graphics: GraphicsBackend | null;
  /**
   * Extra environment variables, added after the compat profile's. Keys match
   * `^[A-Z_][A-Z0-9_]{0,63}$` and are not on the manifest env denylist; values are one line.
   */
  env: Record<string, string>;
};

/** An installed package that launches through Proton or Wine. */
export type PackageCompat = {
  package: PackageRef;
  title: string;
  layer: "proton" | "wine";
  /** Null when the package uses the defaults. */
  override: CompatOverride | null;
};

export type CompatSettingsError =
  | { kind: "not_found" }
  /** Not a runner from the catalog for this computer. */
  | { kind: "unknown_runner" }
  | { kind: "graphics_unavailable"; backend: GraphicsBackend }
  /** `format`: the key isn't `^[A-Z_][A-Z0-9_]{0,63}$` or the value isn't one line; `denied`: the key is reserved. */
  | { kind: "invalid_env"; key: string; reason: "format" | "denied" }
  | { kind: "io"; detail: string };

export type RuntimeRemoveError =
  | { kind: "not_found" }
  /** Installed packages still launch with it. */
  | { kind: "in_use"; used_by: number }
  | { kind: "io"; detail: string };

/** A runtime's license, shipped next to it (Apple's for D3DMetal, 09-compatibility §3). */
export type RuntimeLicense = {
  runtime: RuntimeId;
  /** Display name, e.g. "D3DMetal (Apple Game Porting Toolkit)". */
  name: string;
  /** SPDX expression from the catalog (`LicenseRef-…` for Apple's). */
  spdx: string;
  /** Plain text; rendered as text, never as HTML or Markdown. */
  text: string;
};

export const compatCommands = {
  async compatOverview(): Promise<Result<CompatOverview, AppError>> {
    return call("compat_overview");
  },
  /** Null = automatic. A runner that isn't installed yet downloads at the next launch that needs it. */
  async compatDefaultSet(runner: RunnerVersion | null): Promise<Result<null, CompatSettingsError>> {
    return call("compat_default_set", { runner });
  },
  /** Deletes an unused runtime version from disk. */
  async runtimeRemove(
    runtime: RuntimeId,
    version: string,
  ): Promise<Result<null, RuntimeRemoveError>> {
    return call("runtime_remove", { runtime, version });
  },
  async compatPackages(): Promise<Result<PackageCompat[], AppError>> {
    return call("compat_packages");
  },
  async compatOverrideSet(
    pkg: PackageRef,
    override: CompatOverride,
  ): Promise<Result<null, CompatSettingsError>> {
    return call("compat_override_set", { package: pkg, override });
  },
  /** Back to the defaults (removes the local override). */
  async compatOverrideReset(pkg: PackageRef): Promise<Result<null, AppError>> {
    return call("compat_override_reset", { package: pkg });
  },
  async compatLicenses(): Promise<Result<RuntimeLicense[], AppError>> {
    return call("compat_licenses");
  },
};
