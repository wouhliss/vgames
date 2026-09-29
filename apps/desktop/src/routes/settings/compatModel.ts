// Pure helpers for Settings → Compatibility: runner names and select values, the extra-environment
// editor (one NAME=value per line) and a one-line summary of a package's override.
import { t } from "../../i18n";
import type { CompatOverride, RunnerVersion, RuntimeId } from "../../ipc";

export const COMPAT_OVERVIEW = ["compat_overview"] as const;
export const COMPAT_PACKAGES = ["compat_packages"] as const;

const RUNTIME_NAMES: Record<RuntimeId, string> = {
  "umu-proton": "UMU-Proton",
  "ge-proton": "GE-Proton",
  "umu-launcher": "umu-launcher",
  "wine-macos": "Wine",
  d3dmetal: "D3DMetal",
  dxmt: "DXMT",
  "dxvk-macos": "DXVK-macOS",
  moltenvk: "MoltenVK",
};

/** "UMU-Proton 10.0-2", "GE-Proton10-4" (GE versions already carry the name), "Wine 10.4". */
export function runtimeLabel(runtime: RuntimeId, version: string): string {
  const name = RUNTIME_NAMES[runtime];
  return version.startsWith(name) ? version : `${name} ${version}`;
}

export const runnerLabel = (runner: RunnerVersion) => runtimeLabel(runner.runtime, runner.version);

/** Select values: "<runtime>@<version>". */
export const runnerValue = (runner: RunnerVersion) => `${runner.runtime}@${runner.version}`;

export function findRunner(
  runners: readonly RunnerVersion[],
  value: string,
): RunnerVersion | undefined {
  return runners.find((r) => runnerValue(r) === value);
}

const ENV_KEY = /^[A-Z_][A-Z0-9_]{0,63}$/;

export type EnvParse =
  | { ok: true; env: Record<string, string> }
  | { ok: false; line: number; problem: "format" | "duplicate"; key: string };

/** Parses "NAME=value" lines; blank lines are skipped. The value keeps everything after the first "=". */
export function parseEnv(text: string): EnvParse {
  const env: Record<string, string> = {};
  const lines = text.split("\n");
  for (const [index, raw] of lines.entries()) {
    const line = raw.trim();
    if (line === "") continue;
    const eq = line.indexOf("=");
    const key = eq === -1 ? line : line.slice(0, eq).trim();
    if (eq === -1 || !ENV_KEY.test(key))
      return { ok: false, line: index + 1, problem: "format", key };
    if (Object.hasOwn(env, key)) return { ok: false, line: index + 1, problem: "duplicate", key };
    env[key] = line.slice(eq + 1).trim();
  }
  return { ok: true, env };
}

export const formatEnv = (env: Record<string, string>) =>
  Object.entries(env)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");

export const isDefaultOverride = (o: CompatOverride) =>
  o.runner === null && o.graphics === null && Object.keys(o.env).length === 0;

/** "GE-Proton10-4 · DXMT · 2 environment variables", or "Uses the defaults". */
export function overrideSummary(o: CompatOverride | null): string {
  if (o === null || isDefaultOverride(o)) return t("settings.compat.usesDefaults");
  const parts: string[] = [];
  if (o.runner) parts.push(runnerLabel(o.runner));
  if (o.graphics) parts.push(t(`settings.compat.graphics.${o.graphics}`));
  const count = Object.keys(o.env).length;
  if (count > 0) parts.push(t("settings.compat.envCount", { count }));
  return t("settings.compat.customized", { summary: parts.join(" · ") });
}
