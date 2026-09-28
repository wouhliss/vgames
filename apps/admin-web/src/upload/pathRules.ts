// Path rules (02-package-format §3), checked in the browser before planning so the preview can list
// every invalid path with its reason. pack-wasm checks them again (the server and launcher too).
const RESERVED = new Set([
  "CON",
  "PRN",
  "AUX",
  "NUL",
  "CONIN$",
  "CONOUT$",
  ...Array.from({ length: 10 }, (_, i) => `COM${i}`),
  ...Array.from({ length: 10 }, (_, i) => `LPT${i}`),
  "COM¹",
  "COM²",
  "COM³",
  "LPT¹",
  "LPT²",
  "LPT³",
]);
const FORBIDDEN = /[<>:"|?*\\]/;
/** U+0000–U+001F and U+007F. */
const hasControl = (s: string) =>
  [...s].some((c) => {
    const code = c.codePointAt(0) ?? 0;
    return code < 0x20 || code === 0x7f;
  });
const encoder = new TextEncoder();
const bytes = (s: string) => encoder.encode(s).length;

/** Why `path` is not a valid manifest path, or null. */
export function pathProblem(path: string): string | null {
  if (path.normalize("NFC") !== path) return "not in Unicode NFC form";
  if (path.startsWith("/") || path.includes("\\")) return "must be relative with / separators";
  const parts = path.split("/");
  if (bytes(path) > 512) return "longer than 512 bytes";
  if (parts.length > 64) return "more than 64 folder levels";
  if (parts[0] === ".vgames") return "the .vgames folder is reserved";
  for (const part of parts) {
    if (part === "" || part === "." || part === "..") return "has an empty, . or .. part";
    if (bytes(part) > 255) return `"${part.slice(0, 20)}…" is longer than 255 bytes`;
    if (FORBIDDEN.test(part) || hasControl(part))
      return `"${part}" contains a character Windows can't use (< > : " | ? * or a control character)`;
    if (part.endsWith(".") || part.endsWith(" ")) return `"${part}" ends with a dot or a space`;
    const stem = (part.split(".")[0] ?? "").toUpperCase();
    if (RESERVED.has(stem)) return `"${part}" is a reserved name on Windows`;
  }
  return null;
}

export interface InvalidPath {
  path: string;
  reason: string;
}

/**
 * Every invalid path, including case-insensitive duplicates and a file that is also used as a
 * folder. `files` are paths of files; `dirs` of empty folders.
 */
export function checkTree(files: readonly string[], dirs: readonly string[] = []): InvalidPath[] {
  const out: InvalidPath[] = [];
  const seen = new Map<string, string>();
  for (const path of [...files, ...dirs]) {
    const reason = pathProblem(path);
    if (reason) {
      out.push({ path, reason });
      continue;
    }
    // Simple case folding: toLowerCase of the upper form is close enough for a preview.
    const folded = path.toUpperCase().toLowerCase();
    const other = seen.get(folded);
    if (other !== undefined)
      out.push({ path, reason: `same name as "${other}" apart from letter case` });
    else seen.set(folded, path);
  }
  const fileSet = new Set(files);
  for (const path of files) {
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i += 1) {
      const prefix = parts.slice(0, i).join("/");
      if (fileSet.has(prefix))
        out.push({ path: prefix, reason: `is a file, but "${path}" uses it as a folder` });
    }
  }
  return out;
}
