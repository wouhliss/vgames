// The package form: fields, the same limits as the API (lengths in Unicode code points after
// trimming, like the server), the merge patch built from what changed, and the slug preview.
import type { Schemas } from "@vgames/api-client";
import type { AdminPackage, FieldSource, PackageStatus } from "../../api/schemas";

export const STATUSES: readonly PackageStatus[] = ["draft", "published", "hidden", "archived"];

export const STATUS_LABEL: Record<PackageStatus, string> = {
  draft: "Draft",
  published: "Published",
  hidden: "Hidden",
  archived: "Archived",
};

export const SLUG = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;

/** Code points, as the server counts (`chars().count()`). */
export const length = (s: string) => [...s].length;

/** Mirrors the API's `slugify`: NFKD, marks dropped, ASCII letters/digits, `-` between words, ≤ 56. */
export function slugify(title: string): string {
  let out = "";
  let dash = false;
  for (const c of title.normalize("NFKD")) {
    if (/^[A-Za-z0-9]$/.test(c)) {
      out += c.toLowerCase();
      dash = false;
    } else if (/^\p{M}$/u.test(c)) {
      // combining mark: dropped
    } else if (out.length > 0 && !dash) {
      out += "-";
      dash = true;
    }
  }
  const s = out
    .replace(/^-+|-+$/g, "")
    .slice(0, 56)
    .replace(/^-+|-+$/g, "");
  return s || "package";
}

export type FieldName =
  | "title"
  | "slug"
  | "summary"
  | "description"
  | "developer"
  | "publisher"
  | "release_date"
  | "genres"
  | "steam_app_id"
  | "igdb_id";

export interface FieldSpec {
  name: FieldName;
  label: string;
  kind: "text" | "textarea" | "date" | "id" | "genres";
  /** Required fields can't be cleared. */
  required?: boolean;
  max?: number;
  hint?: string;
}

export const FIELDS: readonly FieldSpec[] = [
  { name: "title", label: "Title", kind: "text", required: true, max: 200 },
  {
    name: "slug",
    label: "Slug",
    kind: "text",
    required: true,
    max: 64,
    hint: "Lowercase letters, digits and dashes. Used in links.",
  },
  { name: "summary", label: "Summary", kind: "textarea", max: 500 },
  {
    name: "description",
    label: "Description",
    kind: "textarea",
    max: 20_000,
    hint: "Plain text or Markdown. HTML is shown as text.",
  },
  { name: "developer", label: "Developer", kind: "text", max: 200 },
  { name: "publisher", label: "Publisher", kind: "text", max: 200 },
  { name: "release_date", label: "Release date", kind: "date" },
  {
    name: "genres",
    label: "Genres",
    kind: "genres",
    hint: "One per line, up to 20, each up to 64 characters.",
  },
  { name: "steam_app_id", label: "Steam app id", kind: "id" },
  { name: "igdb_id", label: "IGDB id", kind: "id" },
];

export type FormValues = Record<FieldName, string>;

export function fromPackage(pkg: AdminPackage): FormValues {
  return {
    title: pkg.title,
    slug: pkg.slug,
    summary: pkg.summary ?? "",
    description: pkg.description ?? "",
    developer: pkg.developer ?? "",
    publisher: pkg.publisher ?? "",
    release_date: pkg.release_date ?? "",
    genres: (pkg.genres ?? []).join("\n"),
    steam_app_id: pkg.steam_app_id ? String(pkg.steam_app_id) : "",
    igdb_id: pkg.igdb_id ? String(pkg.igdb_id) : "",
  };
}

export const splitGenres = (value: string) =>
  value
    .split("\n")
    .map((g) => g.trim())
    .filter((g) => g.length > 0);

/** A positive integer id, `null` for empty, or `undefined` when it is not a valid id. */
export function parseId(value: string): number | null | undefined {
  const v = value.trim();
  if (v === "") return null;
  if (!/^[0-9]{1,15}$/.test(v)) return undefined;
  const n = Number(v);
  return n >= 1 ? n : undefined;
}

/** The first problem with a field's value, as the sentence shown under it. */
export function validateField(spec: FieldSpec, value: string): string | null {
  const trimmed = value.trim();
  if (spec.required && trimmed.length === 0) return `${spec.label} is required.`;
  switch (spec.kind) {
    case "id":
      return parseId(value) === undefined ? "Must be a whole number of at least 1." : null;
    case "date":
      return trimmed && !/^\d{4}-\d{2}-\d{2}$/.test(trimmed) ? "Use the format YYYY-MM-DD." : null;
    case "genres": {
      const genres = splitGenres(value);
      if (genres.length > 20) return `At most 20 genres (you have ${genres.length}).`;
      const long = genres.find((g) => length(g) > 64);
      return long ? `Each genre can be at most 64 characters ("${long.slice(0, 20)}…").` : null;
    }
    default:
      break;
  }
  if (spec.name === "slug" && trimmed && !SLUG.test(trimmed))
    return "Use 1–64 lowercase letters, digits and dashes, not starting or ending with a dash.";
  if (spec.max !== undefined) {
    const n = spec.name === "title" ? length(trimmed) : length(value);
    if (n > spec.max)
      return `At most ${spec.max.toLocaleString("en")} characters (now ${n.toLocaleString("en")}).`;
  }
  return null;
}

export function validateAll(values: FormValues): Partial<Record<FieldName, string>> {
  const errors: Partial<Record<FieldName, string>> = {};
  for (const spec of FIELDS) {
    const error = validateField(spec, values[spec.name]);
    if (error) errors[spec.name] = error;
  }
  return errors;
}

export type Patch = Schemas["AdminPackagePatch"];

/** A JSON Merge Patch with only the fields that differ from `base` (empty optional fields → null). */
export function buildPatch(base: FormValues, values: FormValues): Patch {
  const patch: Patch = {};
  const changed = (name: FieldName) => base[name] !== values[name];
  const text = (v: string) => (v.trim() === "" ? null : v);
  if (changed("title")) patch.title = values.title.trim();
  if (changed("slug")) patch.slug = values.slug.trim();
  if (changed("summary")) patch.summary = text(values.summary);
  if (changed("description")) patch.description = text(values.description);
  if (changed("developer")) patch.developer = text(values.developer.trim());
  if (changed("publisher")) patch.publisher = text(values.publisher.trim());
  if (changed("release_date")) patch.release_date = text(values.release_date.trim());
  if (changed("genres")) patch.genres = splitGenres(values.genres);
  if (changed("steam_app_id")) patch.steam_app_id = parseId(values.steam_app_id) ?? null;
  if (changed("igdb_id")) patch.igdb_id = parseId(values.igdb_id) ?? null;
  return patch;
}

export const isDirty = (base: FormValues, values: FormValues) =>
  FIELDS.some((f) => base[f.name] !== values[f.name]);

/** Fields where someone else's saved value differs from what this form started from. */
export function conflicts(
  started: FormValues,
  latest: FormValues,
  mine: FormValues,
): { field: FieldSpec; theirs: string; mine: string }[] {
  return FIELDS.filter((f) => started[f.name] !== latest[f.name]).map((field) => ({
    field,
    theirs: latest[field.name],
    mine: mine[field.name],
  }));
}

export const SOURCE_LABEL: Record<FieldSource, string> = {
  admin: "Edited by an admin",
  igdb: "From IGDB",
  steam: "From Steam",
};
