// TanStack Query policy for the admin web (A3-T13):
// - never retry 4xx (except 429, which waits for Retry-After);
// - retry network errors, timeouts and 5xx twice with exponential backoff;
// - queries are keyed by their filters, so changing a filter is a different cache entry.
import { MutationCache, QueryCache, QueryClient } from "@tanstack/react-query";
import { ApiError } from "./errors";

const MAX_RETRIES = 2;

export function shouldRetry(failureCount: number, error: unknown): boolean {
  if (!(error instanceof ApiError)) return false;
  return error.transient && failureCount < MAX_RETRIES;
}

export function retryDelay(failureCount: number, error: unknown): number {
  if (
    error instanceof ApiError &&
    error.detail.kind === "http" &&
    error.detail.retryAfterSeconds !== null
  ) {
    return error.detail.retryAfterSeconds * 1000;
  }
  return Math.min(1000 * 2 ** failureCount, 8000);
}

export interface GlobalErrorHandlers {
  /** No response at all (offline, timeout): show the offline banner. (401s: `setUnauthorizedHandler`.) */
  onNetworkError: () => void;
}

/** `retryDelayMs` replaces the backoff (tests: the same retries, without the waiting). */
export function createQueryClient(
  handlers: GlobalErrorHandlers,
  options: { retryDelayMs?: number } = {},
): QueryClient {
  const delay = options.retryDelayMs;
  const onError = (error: unknown) => {
    if (!(error instanceof ApiError)) return;
    if (error.detail.kind === "network" || error.detail.kind === "timeout")
      handlers.onNetworkError();
  };
  return new QueryClient({
    queryCache: new QueryCache({ onError }),
    mutationCache: new MutationCache({ onError }),
    defaultOptions: {
      // networkMode "always": requests are attempted even when the browser claims to be offline; a
      // failure shows the offline banner with Retry instead of silently pausing (A3-T13).
      queries: {
        retry: shouldRetry,
        retryDelay: delay === undefined ? retryDelay : () => delay,
        refetchOnWindowFocus: false,
        staleTime: 10_000,
        networkMode: "always",
      },
      // Writes are never retried automatically: the user decides (idempotency keys protect creates).
      mutations: { retry: false, networkMode: "always" },
    },
  });
}
