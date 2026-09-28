// Google Cloud Storage resumable uploads from the browser (02-package-format §6): start a session
// with the signed start URL, PUT 16 MiB pieces with Content-Range, and ask for the offset
// (`bytes */total`) to resume. Cross-origin: no cookies, no CSRF header.
import type { UploadTarget } from "../api/schemas";

export const REQUEST_TIMEOUT_MS = 120_000;

/**
 * GCS answers "308 Resume Incomplete" without a Location header. Browsers return such a response
 * as is in the default `follow` mode (Fetch: no location, no redirect). Node's fetch (unit tests)
 * treats it as a failed redirect, so the tests switch to `manual`, which Node returns as is.
 */
export const gcsFetchOptions: { redirect: RequestRedirect } = { redirect: "follow" };

export type GcsErrorKind =
  /** No answer: offline, DNS, reset. Retry when the network is back. */
  | "network"
  /** The signed start URL expired or was refused: ask the API for a new one. */
  | "expired"
  /** The session is gone (404/410): start the pack again with a new session. */
  | "gone"
  /** A 5xx or 429 from storage: retry with backoff. */
  | "server"
  /** Anything else: stop and report. */
  | "http";

export class GcsError extends Error {
  constructor(
    readonly kind: GcsErrorKind,
    readonly status: number | null = null,
  ) {
    super(`storage ${kind}${status ? ` (${status})` : ""}`);
  }
}

async function send(url: string, init: RequestInit, signal?: AbortSignal): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  const stop = () => controller.abort();
  signal?.addEventListener("abort", stop, { once: true });
  try {
    return await fetch(url, {
      ...init,
      credentials: "omit",
      redirect: gcsFetchOptions.redirect,
      signal: controller.signal,
    });
  } catch {
    if (signal?.aborted) throw new DOMException("aborted", "AbortError");
    throw new GcsError("network");
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", stop);
  }
}

function classify(status: number): GcsError {
  if (status === 404 || status === 410) return new GcsError("gone", status);
  if (status === 429 || status >= 500) return new GcsError("server", status);
  return new GcsError("http", status);
}

/** Starts a resumable session; resolves to the session URI. */
export async function startSession(target: UploadTarget, signal?: AbortSignal): Promise<string> {
  if (Date.parse(target.expires_at) <= Date.now()) throw new GcsError("expired");
  const res = await send(target.url, { method: target.method, headers: target.headers }, signal);
  if (res.status === 200 || res.status === 201) {
    const location = res.headers.get("Location");
    if (location) return location;
    throw new GcsError("http", res.status);
  }
  if (res.status === 400 || res.status === 401 || res.status === 403)
    throw new GcsError("expired", res.status);
  throw classify(res.status);
}

/** The next byte the session expects, or `total` when the object is complete. */
function confirmed(res: Response, total: number): number {
  if (res.status === 200 || res.status === 201) return total;
  const range = res.headers.get("Range");
  const match = range ? /bytes=0-(\d+)/.exec(range) : null;
  return match ? Number(match[1]) + 1 : 0;
}

export async function queryOffset(
  session: string,
  total: number,
  signal?: AbortSignal,
): Promise<number> {
  const res = await send(
    session,
    { method: "PUT", headers: { "Content-Range": `bytes */${total}` } },
    signal,
  );
  if (res.status === 308 || res.status === 200 || res.status === 201) return confirmed(res, total);
  throw classify(res.status);
}

/** Sends `bytes` at `offset`; resolves to the confirmed offset. */
export async function putPiece(
  session: string,
  bytes: Uint8Array,
  offset: number,
  total: number,
  signal?: AbortSignal,
): Promise<number> {
  const end = offset + bytes.length - 1;
  const res = await send(
    session,
    {
      method: "PUT",
      headers: { "Content-Range": `bytes ${offset}-${end}/${total}` },
      body: bytes as unknown as BodyInit,
    },
    signal,
  );
  if (res.status === 308 || res.status === 200 || res.status === 201) return confirmed(res, total);
  throw classify(res.status);
}

/** A single PUT to a signed URL (the manifest). */
export async function putObject(
  target: UploadTarget,
  bytes: Uint8Array,
  signal?: AbortSignal,
): Promise<void> {
  const res = await send(
    target.url,
    { method: target.method, headers: target.headers, body: bytes as unknown as BodyInit },
    signal,
  );
  if (!res.ok) throw classify(res.status);
}
