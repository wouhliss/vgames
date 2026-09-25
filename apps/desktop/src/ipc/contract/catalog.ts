// The server's catalog as this machine sees it, package details with compatibility, and starting an
// install (Agent 2, A2-T08/T16/T17). Requested shapes; see core.ts for the conventions. The Rust core
// picks the release for this machine (09-compatibility §1) and verifies compat profiles; the UI only
// shows the outcome.
import type { AppError, Platform } from "./core";
import { call, type Result } from "./runtime";

/** How the package would run on this machine, or why it can't (09-compatibility §1). */
export type Availability = "native" | "rosetta" | "proton" | "wine" | "unavailable";

export type CatalogSort = "title" | "recent";

export type CatalogQuery = {
  /** Title search, 0–100 characters (`q`). */
  query: string;
  genre: string | null;
  sort: CatalogSort;
  /** From the previous page; null for the first. */
  cursor: string | null;
};

export type CatalogItem = {
  package_id: string;
  slug: string;
  title: string;
  summary: string | null;
  genres: string[];
  /** `vgimg:` URL served by the Rust core. */
  cover_url: string | null;
  platforms: Platform[];
  availability: Availability;
  updated_at: string;
};

export type CatalogPage = { items: CatalogItem[]; next_cursor: string | null };

export type GenreCount = { genre: string; count: number };

export type CatalogError =
  /** The package doesn't exist anymore, or isn't published. */
  | { kind: "not_found" }
  | { kind: "offline" }
  | { kind: "unauthenticated" }
  | { kind: "server"; code: string; message: string };

export type Screenshot = { url: string; width: number; height: number };

/** The release this machine would install (native first, then a compatibility layer). */
export type HostRelease = {
  platform: Platform;
  version_id: string;
  version_label: string;
  sequence: number;
  /** Bytes of the whole release (the installed size). */
  total_size: number;
  published_at: string;
  via: Availability;
};

export type CompatStatus = "verified" | "playable" | "unsupported" | "untested";
export type ProtonDbTier = "platinum" | "gold" | "silver" | "bronze" | "borked" | "pending";

/** Something that stops (or will stop) a compat launch on this machine (09-compatibility §3). */
export type CompatBlocker =
  /** A DirectX 12 title on an Intel Mac: only D3DMetal runs DX12, and it needs Apple silicon. */
  | { kind: "needs_apple_silicon" }
  /** Rosetta 2 isn't installed; `rosetta_install` installs it after the user confirms. */
  | { kind: "needs_rosetta" }
  /** x86_64-only path that Apple's Rosetta policy may end ("may stop working on macOS 28+"). Not blocking. */
  | { kind: "rosetta_sunset"; last_macos: string };

export type CompatInfo =
  | { kind: "native" }
  /** An Intel macOS build on Apple silicon, through Rosetta 2. */
  | { kind: "rosetta"; blockers: CompatBlocker[] }
  | {
      kind: "compat";
      layer: "proton" | "wine";
      /** From the signed compat profile; `untested` without one. */
      status: CompatStatus;
      /** Plain text from the signed profile. */
      notes: string | null;
      /** Community rating, informational only (never used to decide anything). */
      protondb_tier: ProtonDbTier | null;
      blockers: CompatBlocker[];
    }
  | { kind: "unavailable" };

export type PackageDetails = {
  package_id: string;
  slug: string;
  title: string;
  summary: string | null;
  /** CommonMark from the server's admins; render only through SafeMarkdown. */
  description: string | null;
  developer: string | null;
  publisher: string | null;
  /** YYYY-MM-DD */
  release_date: string | null;
  genres: string[];
  platforms: Platform[];
  cover_url: string | null;
  hero_url: string | null;
  logo_url: string | null;
  screenshots: Screenshot[];
  /** Null when no release can run here (see `compat.kind === "unavailable"`). */
  release: HostRelease | null;
  compat: CompatInfo;
};

export type InstallPlan = {
  package_id: string;
  release: HostRelease;
  /** Bytes to download. */
  download_bytes: number;
  /** Free space needed on the chosen library: total size + 64 MiB (02-package-format §7). */
  required_bytes: number;
};

export type InstallPlanError =
  | { kind: "not_found" }
  /** No build for this machine and no compatibility path. */
  | { kind: "no_release" }
  | { kind: "already_installed" }
  | { kind: "offline" }
  | { kind: "blocked"; blocker: CompatBlocker }
  | { kind: "server"; code: string; message: string };

export type InstallStartError =
  | InstallPlanError
  | { kind: "insufficient_space"; required_bytes: number; available_bytes: number }
  | { kind: "library_offline"; library_path: string }
  /** The server's trust bundle expired: installed games still launch, new installs wait (01-security §3.2). */
  | { kind: "trust_expired" }
  | { kind: "io"; detail: string };

export const catalogCommands = {
  async catalogList(query: CatalogQuery): Promise<Result<CatalogPage, CatalogError>> {
    return call("catalog_list", { query });
  },
  /** Genres in the catalog with package counts (for the filter). */
  async catalogGenres(): Promise<Result<GenreCount[], CatalogError>> {
    return call("catalog_genres");
  },
  async packageDetails(packageId: string): Promise<Result<PackageDetails, CatalogError>> {
    return call("package_details", { packageId });
  },
  /** Sizes for the install dialog. Nothing is downloaded yet. */
  async installPlan(packageId: string): Promise<Result<InstallPlan, InstallPlanError>> {
    return call("install_plan", { packageId });
  },
  /** Queues the install into `libraryId`; progress follows through `install-progress`. */
  async installStart(
    packageId: string,
    libraryId: string,
  ): Promise<Result<null, InstallStartError>> {
    return call("install_start", { packageId, libraryId });
  },
  /** macOS: `softwareupdate --install-rosetta` (the UI asks for confirmation first). */
  async rosettaInstall(): Promise<Result<null, AppError>> {
    return call("rosetta_install");
  },
};
