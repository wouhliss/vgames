// Image upload with progress (fetch can't report upload progress, so this one call uses XHR). Same
// rules as the rest of the API layer: same-origin cookies, the CSRF header, a time limit, typed
// errors from problem+json, and a zod-validated answer. Type and size are checked before sending.
import { ApiError } from "./errors";
import { failure, readCsrfToken } from "./http";
import { type Asset, AssetSchema } from "./schemas";

export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const TYPES = ["image/jpeg", "image/png", "image/webp"] as const;
const UPLOAD_TIMEOUT_MS = 120_000;

/** Sniffs the first bytes: the browser's `file.type` comes from the extension only. */
async function sniff(file: File): Promise<(typeof TYPES)[number] | null> {
  const b = new Uint8Array(await file.slice(0, 12).arrayBuffer());
  if (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return "image/jpeg";
  if (b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47) return "image/png";
  const riff = String.fromCharCode(...b.slice(0, 4));
  const webp = String.fromCharCode(...b.slice(8, 12));
  if (riff === "RIFF" && webp === "WEBP") return "image/webp";
  return null;
}

/** Why this file can't be uploaded, or null. */
export async function checkImage(file: File): Promise<string | null> {
  if (file.size === 0) return "The file is empty.";
  if (file.size > MAX_IMAGE_BYTES)
    return `The file is ${(file.size / 1024 / 1024).toFixed(1)} MiB; images can be at most 10 MiB.`;
  const kind = await sniff(file);
  if (!kind) return "Only JPEG, PNG and WebP images can be uploaded.";
  return null;
}

function headersOf(xhr: XMLHttpRequest): Headers {
  const headers = new Headers();
  for (const line of xhr
    .getAllResponseHeaders()
    .trim()
    .split(/[\r\n]+/)) {
    const i = line.indexOf(":");
    if (i > 0) headers.append(line.slice(0, i).trim(), line.slice(i + 1).trim());
  }
  return headers;
}

export function uploadAsset(
  packageId: string,
  kind: Asset["kind"],
  file: File,
  onProgress: (fraction: number) => void,
  signal?: AbortSignal,
): Promise<Asset> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/v1/admin/packages/${encodeURIComponent(packageId)}/assets`);
    xhr.timeout = UPLOAD_TIMEOUT_MS;
    const token = readCsrfToken();
    if (token) xhr.setRequestHeader("X-CSRF-Token", token);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && e.total > 0) onProgress(e.loaded / e.total);
    };
    xhr.onerror = () => reject(new ApiError({ kind: "network" }));
    xhr.ontimeout = () => reject(new ApiError({ kind: "timeout", afterMs: UPLOAD_TIMEOUT_MS }));
    xhr.onabort = () => reject(new ApiError({ kind: "aborted" }));
    xhr.onload = () => {
      const headers = headersOf(xhr);
      const requestId = headers.get("X-Request-Id");
      if (xhr.status < 200 || xhr.status > 299) {
        const response = new Response(null, {
          status: xhr.status,
          statusText: xhr.statusText,
          headers,
        });
        reject(failure(response, xhr.responseText));
        return;
      }
      let json: unknown;
      try {
        json = JSON.parse(xhr.responseText);
      } catch {
        reject(new ApiError({ kind: "malformed", status: xhr.status, requestId }));
        return;
      }
      const parsed = AssetSchema.safeParse(json);
      if (!parsed.success) {
        reject(
          new ApiError({
            kind: "schema",
            status: xhr.status,
            requestId,
            issues: parsed.error.issues.map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`),
          }),
        );
        return;
      }
      onProgress(1);
      resolve(parsed.data);
    };
    signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    const body = new FormData();
    body.append("kind", kind);
    body.append("file", file);
    xhr.send(body);
  });
}
