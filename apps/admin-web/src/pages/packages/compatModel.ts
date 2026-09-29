// The compat profile editor's model: form fields, the rules of `vgames-core::compat` (checked here
// so the admin sees every problem before signing; the server checks again), and the document.
import type { CompatStatus } from "../../api/schemas";

export type Target = "linux" | "macos";
export type Graphics = "d3dmetal" | "dxmt" | "dxvk" | "wined3d";
export const GRAPHICS: readonly Graphics[] = ["d3dmetal", "dxmt", "dxvk", "wined3d"];

/** `vgames-core::compat::WINETRICKS_ALLOWLIST`. */
export const WINETRICKS = [
  "amstream",
  "corefonts",
  "d3dcompiler_42",
  "d3dcompiler_43",
  "d3dcompiler_46",
  "d3dcompiler_47",
  "d3dx10",
  "d3dx10_43",
  "d3dx11_42",
  "d3dx11_43",
  "d3dx9",
  "d3dx9_43",
  "devenum",
  "dinput8",
  "directplay",
  "dotnet20",
  "dotnet35",
  "dotnet40",
  "dotnet45",
  "dotnet452",
  "dotnet46",
  "dotnet461",
  "dotnet462",
  "dotnet472",
  "dotnet48",
  "dotnetdesktop6",
  "dotnetdesktop7",
  "dotnetdesktop8",
  "faudio",
  "gdiplus",
  "mfc140",
  "mfc42",
  "msxml3",
  "msxml6",
  "openal",
  "physx",
  "quartz",
  "vb6run",
  "vcrun2005",
  "vcrun2008",
  "vcrun2010",
  "vcrun2012",
  "vcrun2013",
  "vcrun2015",
  "vcrun2017",
  "vcrun2019",
  "vcrun2022",
  "wmp9",
  "xact",
  "xact_x64",
  "xna31",
  "xna40",
] as const;

export interface CompatForm {
  status: CompatStatus;
  notes: string;
  platform: "windows-x86_64" | "windows-aarch64";
  minSequence: string;
  maxSequence: string;
  /** Runtime ids, comma separated, most preferred first. */
  prefer: string;
  minVersion: string;
  umuGameId: string;
  /** macOS only, in order. */
  graphics: Graphics[];
  /** `NAME=value` per line. */
  env: string;
  /** `name=mode` per line. */
  dllOverrides: string;
  winetricks: string[];
}

export function emptyForm(target: Target): CompatForm {
  return {
    status: "untested",
    notes: "",
    platform: "windows-x86_64",
    minSequence: "1",
    maxSequence: "",
    prefer: target === "linux" ? "umu-proton" : "",
    minVersion: "",
    umuGameId: "",
    graphics: target === "macos" ? ["d3dmetal", "dxmt", "dxvk", "wined3d"] : [],
    env: "",
    dllOverrides: "",
    winetricks: [],
  };
}

interface Document {
  format: "vgames.compat/1";
  server_id: string;
  package_id: string;
  target: Target;
  revision: number;
  created_at: string;
  applies_to: { platform: string; min_sequence: number; max_sequence: number | null };
  status: CompatStatus;
  notes?: string;
  runner: {
    kind: "proton" | "wine";
    prefer: string[];
    min_version?: string;
    umu_game_id?: string;
    graphics?: Graphics[];
    env?: Record<string, string>;
    dll_overrides?: Record<string, string>;
    winetricks?: string[];
  };
}

/** Fills the form from a stored document (the latest revision). */
export function formFromDocument(json: unknown, target: Target): CompatForm {
  const d = json as Partial<Document>;
  const r = d.runner;
  const lines = (m: Record<string, string> | undefined) =>
    Object.entries(m ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join("\n");
  return {
    ...emptyForm(target),
    status: d.status ?? "untested",
    notes: d.notes ?? "",
    platform: d.applies_to?.platform === "windows-aarch64" ? "windows-aarch64" : "windows-x86_64",
    minSequence: String(d.applies_to?.min_sequence ?? 1),
    maxSequence: d.applies_to?.max_sequence ? String(d.applies_to.max_sequence) : "",
    prefer: (r?.prefer ?? []).join(", "),
    minVersion: r?.min_version ?? "",
    umuGameId: r?.umu_game_id ?? "",
    graphics: target === "macos" ? (r?.graphics ?? []) : [],
    env: lines(r?.env),
    dllOverrides: lines(r?.dll_overrides),
    winetricks: r?.winetricks ?? [],
  };
}

const RUNTIME_ID = /^[a-z0-9][a-z0-9-]{0,63}$/;
const VERSION = /^[A-Za-z0-9._+-]{1,64}$/;
const UMU = /^umu-[A-Za-z0-9._-]{1,64}$/;
const ENV_KEY = /^[A-Z_][A-Z0-9_]{0,63}$/;
const DLL = /^[a-z0-9_.-]{1,64}$/;
const DLL_MODE = /^$|^(native|builtin|n|b)(,(native|builtin|n|b))?$/;
const DENIED_ENV =
  /^(PATH|LD_PRELOAD|LD_LIBRARY_PATH|PYTHONPATH|COMSPEC|WINEPREFIX|WINEDLLOVERRIDES|WINEDLLPATH|WINEPATH|WINELOADER|WINESERVER|PROTONPATH|GAMEID|STORE)$|^(DYLD_|STEAM_COMPAT_|UMU_|PRESSURE_VESSEL_)/;

function pairs(text: string): { key: string; value: string; line: number }[] {
  return text
    .split("\n")
    .map((raw, i) => ({ raw: raw.trim(), line: i + 1 }))
    .filter((l) => l.raw !== "")
    .map(({ raw, line }) => {
      const eq = raw.indexOf("=");
      return eq < 0
        ? { key: raw, value: "\u0000", line }
        : { key: raw.slice(0, eq), value: raw.slice(eq + 1), line };
    });
}

export type FormErrors = Partial<Record<keyof CompatForm, string>>;

export function validate(form: CompatForm, target: Target): FormErrors {
  const e: FormErrors = {};
  if ([...form.notes].length > 2000) e.notes = "At most 2,000 characters.";
  const control = [...form.notes].some((c) => {
    const n = c.codePointAt(0) ?? 0;
    return (n < 0x20 && c !== "\n" && c !== "\t") || n === 0x7f;
  });
  if (control) e.notes = "Plain text only (no control characters).";
  const min = Number(form.minSequence);
  if (!/^[0-9]+$/.test(form.minSequence) || min < 1)
    e.minSequence = "A whole number of at least 1.";
  if (
    form.maxSequence !== "" &&
    (!/^[0-9]+$/.test(form.maxSequence) || Number(form.maxSequence) < min)
  )
    e.maxSequence = "Empty, or a whole number not below the first sequence.";
  const prefer = form.prefer
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const bad = prefer.find((p) => !RUNTIME_ID.test(p));
  if (prefer.length > 8) e.prefer = "At most 8 runtimes.";
  else if (bad) e.prefer = `"${bad}" isn't a runtime id (lowercase letters, digits and dashes).`;
  else if (new Set(prefer).size !== prefer.length) e.prefer = "A runtime is listed twice.";
  if (form.minVersion && !VERSION.test(form.minVersion))
    e.minVersion = "Letters, digits and . _ + - only (up to 64).";
  if (target === "linux" && form.umuGameId && !UMU.test(form.umuGameId))
    e.umuGameId = "A umu id starts with umu-.";
  for (const { key, value, line } of pairs(form.env)) {
    if (value === "\u0000") e.env = `Line ${line}: use NAME=value.`;
    else if (!ENV_KEY.test(key)) e.env = `Line ${line}: ${key} isn't a valid name (A–Z, 0–9, _).`;
    else if (DENIED_ENV.test(key))
      e.env = `Line ${line}: ${key} is set by the launcher and can't be changed.`;
    if (e.env) break;
  }
  if (!e.env && new Set(pairs(form.env).map((p) => p.key)).size !== pairs(form.env).length)
    e.env = "A name is listed twice.";
  const dlls = pairs(form.dllOverrides);
  if (dlls.length > 64) e.dllOverrides = "At most 64 overrides.";
  for (const { key, value, line } of dlls) {
    if (value === "\u0000" || !DLL.test(key) || !DLL_MODE.test(value))
      e.dllOverrides = `Line ${line}: use name=mode, with mode native, builtin, n, b, two of them, or nothing.`;
    if (e.dllOverrides) break;
  }
  if (form.winetricks.length > 32) e.winetricks = "At most 32 verbs.";
  return e;
}

/** The exact document bytes to sign (09 §4). */
export function buildDocument(
  form: CompatForm,
  ids: { serverId: string; packageId: string; target: Target; revision: number; now: Date },
): Uint8Array {
  const map = (text: string) => Object.fromEntries(pairs(text).map((p) => [p.key, p.value]));
  const env = map(form.env);
  const dll = map(form.dllOverrides);
  const doc: Document = {
    format: "vgames.compat/1",
    server_id: ids.serverId,
    package_id: ids.packageId,
    target: ids.target,
    revision: ids.revision,
    created_at: `${ids.now.toISOString().slice(0, 19)}Z`,
    applies_to: {
      platform: form.platform,
      min_sequence: Number(form.minSequence),
      max_sequence: form.maxSequence ? Number(form.maxSequence) : null,
    },
    status: form.status,
    ...(form.notes.trim() ? { notes: form.notes.trim() } : {}),
    runner: {
      kind: ids.target === "linux" ? "proton" : "wine",
      prefer: form.prefer
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
      ...(form.minVersion ? { min_version: form.minVersion } : {}),
      ...(ids.target === "linux" && form.umuGameId ? { umu_game_id: form.umuGameId } : {}),
      ...(ids.target === "macos" && form.graphics.length > 0 ? { graphics: form.graphics } : {}),
      ...(Object.keys(env).length > 0 ? { env } : {}),
      ...(Object.keys(dll).length > 0 ? { dll_overrides: dll } : {}),
      ...(form.winetricks.length > 0 ? { winetricks: form.winetricks } : {}),
    },
  };
  return new TextEncoder().encode(JSON.stringify(doc));
}

export function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

export function fromBase64(text: string): Uint8Array {
  return Uint8Array.from(atob(text), (c) => c.charCodeAt(0));
}
