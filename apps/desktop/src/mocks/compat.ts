// Compatibility runtimes, the default runner and per-package overrides (09-compatibility §2–§5). The
// host and Rosetta 2 come from the catalog state, so one `host` switch changes the whole section.
import type {
  AppError,
  CompatHost,
  CompatOverride,
  CompatSettingsError,
  GraphicsBackend,
  InstalledPackage,
  InstalledRuntime,
  PackageCompat,
  Platform,
  RunnerVersion,
  RuntimeLicense,
  RuntimeRemoveError,
} from "../ipc";
import { fail, type Handler } from "./runtime";

type HostOs = "linux" | "macos";

export interface CompatState {
  /** Runtime versions on disk, for either OS (the mock shows the host's). */
  runtimes: (InstalledRuntime & { os: HostOs })[];
  /** Runner versions in the runtime catalog, newest first. */
  runners: (RunnerVersion & { os: HostOs })[];
  defaultRunner: RunnerVersion | null;
  /** Local overrides by `<server id>/<package id>`. */
  compatOverrides: Record<string, CompatOverride>;
  rosettaLastMacos: string | null;
  runtimeLicenses: (RuntimeLicense & { os: HostOs })[];
}

const GIB = 1024 ** 3;
const MIB = 1024 ** 2;

export function defaultCompatState(): CompatState {
  return {
    runtimes: [
      { os: "linux", runtime: "umu-launcher", version: "1.2.9", size_bytes: 12 * MIB, used_by: 3 },
      { os: "linux", runtime: "umu-proton", version: "9.0-4", size_bytes: 1.4 * GIB, used_by: 3 },
      {
        os: "linux",
        runtime: "ge-proton",
        version: "GE-Proton9-27",
        size_bytes: 1.3 * GIB,
        used_by: 0,
      },
      { os: "macos", runtime: "wine-macos", version: "10.0", size_bytes: 820 * MIB, used_by: 2 },
      { os: "macos", runtime: "d3dmetal", version: "2.1", size_bytes: 96 * MIB, used_by: 2 },
      { os: "macos", runtime: "dxmt", version: "0.60", size_bytes: 18 * MIB, used_by: 0 },
    ],
    runners: [
      { os: "linux", runtime: "umu-proton", version: "10.0-2" },
      { os: "linux", runtime: "umu-proton", version: "9.0-4" },
      { os: "linux", runtime: "ge-proton", version: "GE-Proton10-4" },
      { os: "linux", runtime: "ge-proton", version: "GE-Proton9-27" },
      { os: "macos", runtime: "wine-macos", version: "10.4" },
      { os: "macos", runtime: "wine-macos", version: "10.0" },
    ],
    defaultRunner: null,
    compatOverrides: {},
    rosettaLastMacos: "27",
    runtimeLicenses: [
      {
        os: "linux",
        runtime: "umu-proton",
        name: "UMU-Proton",
        spdx: "BSD-3-Clause AND LGPL-2.1-or-later",
        text: "Proton\n\nCopyright (c) Valve Corporation and contributors.\n\nRedistribution and use in source and binary forms, with or without modification, are permitted provided that the conditions in the license are met.",
      },
      {
        os: "linux",
        runtime: "umu-launcher",
        name: "umu-launcher",
        spdx: "GPL-3.0-only",
        text: "umu-launcher\n\nThis program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License, version 3.",
      },
      {
        os: "macos",
        runtime: "d3dmetal",
        name: "D3DMetal (Apple Game Porting Toolkit)",
        spdx: "LicenseRef-Apple-GPTK",
        text: "Apple Game Porting Toolkit license\n\n(The full license text ships next to the D3DMetal framework and is shown here unchanged.)",
      },
      {
        os: "macos",
        runtime: "wine-macos",
        name: "Wine",
        spdx: "LGPL-2.1-or-later",
        text: "Wine\n\nThis library is free software; you can redistribute it and/or modify it under the terms of the GNU Lesser General Public License, version 2.1 or later.",
      },
    ],
  };
}

/** Keys the Rust core refuses (the manifest env denylist, 02-package-format §5). */
const DENIED_ENV = /^(PATH|LD_PRELOAD|LD_LIBRARY_PATH|DYLD_.*|PYTHONPATH|COMSPEC)$/;
const ENV_KEY = /^[A-Z_][A-Z0-9_]{0,63}$/;

const osOf = (host: Platform): HostOs | null =>
  host.startsWith("linux") ? "linux" : host.startsWith("macos") ? "macos" : null;

const key = (ref: { server_id: string; package_id: string }) =>
  `${ref.server_id}/${ref.package_id}`;

const sameRunner = (a: RunnerVersion, b: RunnerVersion) =>
  a.runtime === b.runtime && a.version === b.version;

export function compatHandlers(
  state: CompatState & { host: Platform; rosettaInstalled: boolean; installs: InstalledPackage[] },
): Record<string, Handler> {
  const host = (): CompatHost => {
    const os = osOf(state.host);
    if (os === "linux") return { kind: "proton" };
    if (os === null) return { kind: "native" };
    const appleSilicon = state.host === "macos-aarch64";
    return {
      kind: "wine",
      apple_silicon: appleSilicon,
      rosetta: appleSilicon ? (state.rosettaInstalled ? "installed" : "missing") : null,
      rosetta_last_macos: state.rosettaLastMacos,
    };
  };
  const strip = <T extends { os: HostOs }>({ os: _os, ...rest }: T) => rest;
  const forHost = <T extends { os: HostOs }>(list: T[]) =>
    list.filter((item) => item.os === osOf(state.host)).map(strip);
  const graphics = (): GraphicsBackend[] =>
    state.host === "macos-aarch64"
      ? ["d3dmetal", "dxmt", "dxvk", "wined3d"]
      : state.host === "macos-x86_64"
        ? ["dxmt", "dxvk", "wined3d"]
        : [];
  const knownRunner = (runner: RunnerVersion) =>
    forHost(state.runners).some((r) => sameRunner(r, runner));

  return {
    compat_overview: () => ({
      host: host(),
      runtimes: forHost(state.runtimes),
      runners: forHost(state.runners),
      graphics: graphics(),
      default_runner: state.defaultRunner,
    }),
    compat_default_set: (args) => {
      const runner = (args.runner ?? null) as RunnerVersion | null;
      if (runner && !knownRunner(runner))
        fail({ kind: "unknown_runner" } satisfies CompatSettingsError);
      state.defaultRunner = runner;
      return null;
    },
    runtime_remove: (args) => {
      const target = state.runtimes.find(
        (r) => r.runtime === args.runtime && r.version === args.version,
      );
      if (!target) fail({ kind: "not_found" } satisfies RuntimeRemoveError);
      if (target.used_by > 0)
        fail({ kind: "in_use", used_by: target.used_by } satisfies RuntimeRemoveError);
      state.runtimes = state.runtimes.filter((r) => r !== target);
      return null;
    },
    // The layer follows the host (fixtures mark Windows builds "proton" whatever the host).
    compat_packages: (): PackageCompat[] => {
      const os = osOf(state.host);
      if (os === null) return [];
      return state.installs
        .filter((i) => i.compat !== "native")
        .map((i) => ({
          package: i.package,
          title: i.title,
          layer: os === "macos" ? "wine" : "proton",
          override: state.compatOverrides[key(i.package)] ?? null,
        }));
    },
    compat_override_set: (args) => {
      const ref = args.package as { server_id: string; package_id: string };
      if (!state.installs.some((i) => key(i.package) === key(ref) && i.compat !== "native"))
        fail({ kind: "not_found" } satisfies CompatSettingsError);
      const next = args.override as CompatOverride;
      if (next.runner && !knownRunner(next.runner))
        fail({ kind: "unknown_runner" } satisfies CompatSettingsError);
      if (next.graphics && !graphics().includes(next.graphics))
        fail({
          kind: "graphics_unavailable",
          backend: next.graphics,
        } satisfies CompatSettingsError);
      for (const [name, value] of Object.entries(next.env)) {
        if (!ENV_KEY.test(name) || /[\r\n\0]/.test(value))
          fail({ kind: "invalid_env", key: name, reason: "format" } satisfies CompatSettingsError);
        if (DENIED_ENV.test(name))
          fail({ kind: "invalid_env", key: name, reason: "denied" } satisfies CompatSettingsError);
      }
      state.compatOverrides = { ...state.compatOverrides, [key(ref)]: next };
      return null;
    },
    compat_override_reset: (args) => {
      const ref = args.package as { server_id: string; package_id: string };
      if (!state.installs.some((i) => key(i.package) === key(ref)))
        fail({ kind: "not_found" } satisfies AppError);
      const { [key(ref)]: _removed, ...rest } = state.compatOverrides;
      state.compatOverrides = rest;
      return null;
    },
    compat_licenses: () => forHost(state.runtimeLicenses),
  };
}
