// Admin package endpoints (openapi.yaml, admin-packages). Every body is validated with zod.
import type { Schemas } from "@vgames/api-client";
import { call, callEmpty, client } from "./http";
import {
  AdminPackagePageSchema,
  AdminPackageSchema,
  CandidatesSchema,
  JobSchema,
  type PackageStatus,
} from "./schemas";

export interface PackageFilters {
  q: string;
  status: PackageStatus | "";
}

export const packageKeys = {
  list: (filters: PackageFilters) => ["admin-packages", filters] as const,
  one: (id: string) => ["admin-package", id] as const,
};

export function listPackages(filters: PackageFilters, cursor: string | null, signal?: AbortSignal) {
  const query: { limit: number; q?: string; status?: PackageStatus; cursor?: string } = {
    limit: 50,
  };
  if (filters.q.trim()) query.q = filters.q.trim();
  if (filters.status) query.status = filters.status;
  if (cursor) query.cursor = cursor;
  return call(
    AdminPackagePageSchema,
    (o) => client.GET("/v1/admin/packages", { ...o, params: { query } }),
    signal,
  );
}

export function getPackage(id: string, signal?: AbortSignal) {
  return call(
    AdminPackageSchema,
    (o) =>
      client.GET("/v1/admin/packages/{package_id}", { ...o, params: { path: { package_id: id } } }),
    signal,
  );
}

export function createPackage(body: Schemas["AdminPackageCreate"], idempotencyKey: string) {
  return call(AdminPackageSchema, (o) =>
    client.POST("/v1/admin/packages", {
      ...o,
      params: { header: { "Idempotency-Key": idempotencyKey } },
      body,
    }),
  );
}

export function updatePackage(id: string, etag: string, patch: Schemas["AdminPackagePatch"]) {
  return call(AdminPackageSchema, (o) =>
    client.PATCH("/v1/admin/packages/{package_id}", {
      ...o,
      params: { path: { package_id: id }, header: { "If-Match": etag } },
      body: patch,
      headers: { "Content-Type": "application/merge-patch+json" },
    }),
  );
}

export function deletePackage(id: string, etag: string) {
  return callEmpty((o) =>
    client.DELETE("/v1/admin/packages/{package_id}", {
      ...o,
      params: { path: { package_id: id }, header: { "If-Match": etag } },
    }),
  );
}

// ---------------------------------------------------------------- metadata and images (A3-T15)

export const metadataKeys = {
  candidates: (id: string) => ["admin-package-candidates", id] as const,
};

export function listCandidates(id: string, signal?: AbortSignal) {
  return call(
    CandidatesSchema,
    (o) =>
      client.GET("/v1/admin/packages/{package_id}/metadata/candidates", {
        ...o,
        params: { path: { package_id: id } },
      }),
    signal,
  );
}

export function refreshMetadata(id: string) {
  return call(JobSchema, (o) =>
    client.POST("/v1/admin/packages/{package_id}/metadata/refresh", {
      ...o,
      params: { path: { package_id: id } },
    }),
  );
}

export function applyMetadata(id: string, etag: string, body: Schemas["MetadataApply"]) {
  return call(AdminPackageSchema, (o) =>
    client.POST("/v1/admin/packages/{package_id}/metadata/apply", {
      ...o,
      params: { path: { package_id: id }, header: { "If-Match": etag } },
      body,
    }),
  );
}

export function deleteAsset(assetId: string) {
  return callEmpty((o) =>
    client.DELETE("/v1/admin/assets/{asset_id}", { ...o, params: { path: { asset_id: assetId } } }),
  );
}
