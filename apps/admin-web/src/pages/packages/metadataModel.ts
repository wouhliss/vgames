// Metadata review: which candidate fields can be applied, how they compare with the current values,
// and what is ticked by default (fields the candidate has, except ones an admin edited).
import type { Schemas } from "@vgames/api-client";
import type { AdminPackage, MetadataCandidate } from "../../api/schemas";

export type ApplyField = Schemas["MetadataApply"]["fields"][number];

export const APPLY_FIELDS: readonly { name: ApplyField; label: string }[] = [
  { name: "title", label: "Title" },
  { name: "summary", label: "Summary" },
  { name: "description", label: "Description" },
  { name: "release_date", label: "Release date" },
  { name: "developer", label: "Developer" },
  { name: "publisher", label: "Publisher" },
  { name: "genres", label: "Genres" },
  { name: "cover", label: "Cover image" },
  { name: "hero", label: "Hero image" },
  { name: "logo", label: "Logo" },
  { name: "screenshots", label: "Screenshots" },
  { name: "external_ids", label: "Steam / IGDB ids" },
];

const none = "";

export function currentValue(pkg: AdminPackage, field: ApplyField): string {
  switch (field) {
    case "genres":
      return (pkg.genres ?? []).join(", ");
    case "cover":
    case "hero":
    case "logo":
      return pkg[field]
        ? `${pkg[field].width}×${pkg[field].height} (${pkg[field].source ?? "upload"})`
        : none;
    case "screenshots":
      return pkg.screenshots?.length ? `${pkg.screenshots.length} screenshots` : none;
    case "external_ids":
      return [
        pkg.steam_app_id ? `Steam ${pkg.steam_app_id}` : "",
        pkg.igdb_id ? `IGDB ${pkg.igdb_id}` : "",
      ]
        .filter(Boolean)
        .join(", ");
    default:
      return pkg[field] ?? none;
  }
}

export function candidateValue(c: MetadataCandidate, field: ApplyField): string {
  const d = c.data;
  switch (field) {
    case "genres":
      return (d.genres ?? []).join(", ");
    case "cover":
    case "hero":
    case "logo":
      return d.images?.[field] ?? none;
    case "screenshots":
      return d.images?.screenshots?.length ? `${d.images.screenshots.length} screenshots` : none;
    case "external_ids":
      return [
        d.external?.steam_app_id ? `Steam ${d.external.steam_app_id}` : "",
        d.external?.igdb_id ? `IGDB ${d.external.igdb_id}` : "",
        d.external?.umu_id ? `umu ${d.external.umu_id}` : "",
      ]
        .filter(Boolean)
        .join(", ");
    default:
      return d[field] ?? none;
  }
}

/** Field-source keys the server records for each apply field. */
const SOURCE_KEYS: Record<ApplyField, string[]> = {
  title: ["title"],
  summary: ["summary"],
  description: ["description"],
  release_date: ["release_date"],
  developer: ["developer"],
  publisher: ["publisher"],
  genres: ["genres"],
  cover: ["cover", "cover_asset_id"],
  hero: ["hero", "hero_asset_id"],
  logo: ["logo", "logo_asset_id"],
  screenshots: ["screenshots"],
  external_ids: ["steam_app_id", "igdb_id"],
};

export const editedByAdmin = (pkg: AdminPackage, field: ApplyField) =>
  SOURCE_KEYS[field].some((k) => pkg.field_sources[k] === "admin");

/** Ticked by default: the candidate has a value that differs, and no admin edited the field. */
export function defaultSelection(pkg: AdminPackage, c: MetadataCandidate): Set<ApplyField> {
  return new Set(
    APPLY_FIELDS.map((f) => f.name).filter((name) => {
      const theirs = candidateValue(c, name);
      return theirs !== "" && theirs !== currentValue(pkg, name) && !editedByAdmin(pkg, name);
    }),
  );
}

export const candidateKey = (c: MetadataCandidate) => `${c.source}:${c.external_id}`;
