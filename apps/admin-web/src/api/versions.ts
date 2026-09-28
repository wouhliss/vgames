// Version endpoints (02-package-format §6). Every body is validated with zod.
import type { Schemas } from "@vgames/api-client";
import { call, callEmpty, client } from "./http";
import { type Platform, UploadTargetSchema, VersionPageSchema, VersionSchema } from "./schemas";

export const versionKeys = {
  list: (packageId: string) => ["admin-versions", packageId] as const,
  one: (versionId: string) => ["admin-version", versionId] as const,
};

export function listVersions(packageId: string, cursor: string | null, signal?: AbortSignal) {
  return call(
    VersionPageSchema,
    (o) =>
      client.GET("/v1/admin/packages/{package_id}/versions", {
        ...o,
        params: {
          path: { package_id: packageId },
          query: { limit: 50, ...(cursor ? { cursor } : {}) },
        },
      }),
    signal,
  );
}

export function createVersion(
  packageId: string,
  body: { platform: Platform; version_label: string },
  idempotencyKey: string,
) {
  return call(VersionSchema, (o) =>
    client.POST("/v1/admin/packages/{package_id}/versions", {
      ...o,
      params: { path: { package_id: packageId }, header: { "Idempotency-Key": idempotencyKey } },
      body,
    }),
  );
}

export function getVersion(versionId: string, signal?: AbortSignal) {
  return call(
    VersionSchema,
    (o) =>
      client.GET("/v1/admin/versions/{version_id}", {
        ...o,
        params: { path: { version_id: versionId } },
      }),
    signal,
  );
}

export function abortVersion(versionId: string) {
  return callEmpty((o) =>
    client.DELETE("/v1/admin/versions/{version_id}", {
      ...o,
      params: { path: { version_id: versionId } },
    }),
  );
}

export function packUploadSession(versionId: string, packIndex: number) {
  return call(UploadTargetSchema, (o) =>
    client.POST("/v1/admin/versions/{version_id}/packs/{pack_index}/upload-session", {
      ...o,
      params: { path: { version_id: versionId, pack_index: packIndex } },
    }),
  );
}

export function manifestUpload(versionId: string) {
  return call(UploadTargetSchema, (o) =>
    client.POST("/v1/admin/versions/{version_id}/manifest-upload", {
      ...o,
      params: { path: { version_id: versionId } },
    }),
  );
}

export function finalizeVersion(versionId: string, body: Schemas["FinalizeRequest"]) {
  return call(VersionSchema, (o) =>
    client.POST("/v1/admin/versions/{version_id}/finalize", {
      ...o,
      params: { path: { version_id: versionId } },
      body,
    }),
  );
}

export function publishVersion(versionId: string) {
  return call(VersionSchema, (o) =>
    client.POST("/v1/admin/versions/{version_id}/publish", {
      ...o,
      params: { path: { version_id: versionId } },
    }),
  );
}

export function yankVersion(versionId: string, reason: string) {
  return call(VersionSchema, (o) =>
    client.POST("/v1/admin/versions/{version_id}/yank", {
      ...o,
      params: { path: { version_id: versionId } },
      body: { reason },
    }),
  );
}

/** Human explanations for the 422 codes `finalize` can answer (apps/api/src/finalize.rs). */
export const FINALIZE_ERRORS: Record<string, string> = {
  signature_invalid:
    "The signature doesn't match the manifest. Sign again with the same key file; if it keeps failing, the key file may be damaged.",
  publisher_key_untrusted:
    "This key isn't in the server's trust bundle (or was revoked). Ask the owner to add it, or use another publisher key.",
  publisher_key_not_yours: "This publisher key belongs to another admin. Use your own key file.",
  publisher_key_expired: "This publisher key has expired or isn't valid yet. Use a current key.",
  manifest_hash_mismatch:
    "The manifest on the server isn't the one that was signed. Upload the manifest again, then finalize.",
  manifest_missing: "The manifest wasn't uploaded. Upload it again, then finalize.",
  manifest_invalid:
    "The manifest was refused as invalid. This is a bug in the uploader; report it with the details.",
  manifest_mismatch: "The manifest is for another version or package. Start a new upload.",
  pack_missing: "Some packs haven't finished uploading. Resume the upload.",
  pack_size_mismatch:
    "Some uploaded packs don't have the expected size. Resume the upload to re-send them.",
  wrong_server: "The manifest names another server. Start a new upload on this server.",
};
