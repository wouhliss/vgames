// One place that turns any failed request into a clear message and the right action.
import { useEffect, useState } from "react";
import { ApiError } from "../api/errors";

export function describeApiError(error: unknown): { title: string; text: string } {
  if (!(error instanceof ApiError)) {
    return {
      title: "Something went wrong",
      text: error instanceof Error ? error.message : "Unknown error.",
    };
  }
  const d = error.detail;
  switch (d.kind) {
    case "network":
      return {
        title: "Can't reach the server",
        text: "Check your connection. Nothing was changed.",
      };
    case "timeout":
      return {
        title: "The server took too long",
        text: `No answer after ${Math.round(d.afterMs / 1000)} s. Nothing may have been changed; check before retrying.`,
      };
    case "aborted":
      return { title: "Cancelled", text: "The request was cancelled." };
    case "malformed":
      return {
        title: "Unexpected response",
        text: `The server answered with something that isn't JSON (request ${d.requestId ?? "unknown"}).`,
      };
    case "schema":
      return {
        title: "Unexpected response",
        text: `The server's answer doesn't match what this page expects. The server and this page may be different versions (request ${d.requestId ?? "unknown"}).`,
      };
    case "http": {
      const p = d.problem;
      const ref = d.requestId ?? p.request_id;
      const suffix = ref ? ` (request ${ref})` : "";
      switch (d.status) {
        case 401:
          return { title: "Signed out", text: "Your session ended. Sign in again to continue." };
        case 403:
          return {
            title: "Not allowed",
            text: `${p.detail ?? "Your role doesn't allow this."}${suffix}`,
          };
        case 404:
          return {
            title: "Not found",
            text: `It may have been deleted, or the link is wrong.${suffix}`,
          };
        case 409:
          return { title: "Conflict", text: `${p.detail ?? p.title}${suffix}` };
        case 412:
          return {
            title: "Changed by someone else",
            text: "This was modified since you loaded it. Reload to see the latest version.",
          };
        case 428:
          return {
            title: "Reload required",
            text: "This change needs the current version. Reload the page and try again.",
          };
        case 429:
          return {
            title: "Too many requests",
            text:
              d.retryAfterSeconds !== null
                ? `Wait ${d.retryAfterSeconds} s and try again.`
                : "Wait a moment and try again.",
          };
        default:
          if (d.status >= 500)
            return { title: "Server error", text: `The server failed to handle this.${suffix}` };
          return { title: p.title, text: `${p.detail ?? ""}${suffix}` };
      }
    }
  }
}

/** Counts down a 429 Retry-After so the user sees when retrying makes sense. */
function useCountdown(seconds: number | null): number | null {
  const [left, setLeft] = useState(seconds);
  useEffect(() => {
    setLeft(seconds);
    if (seconds === null) return;
    const id = setInterval(() => setLeft((s) => (s === null || s <= 1 ? 0 : s - 1)), 1000);
    return () => clearInterval(id);
  }, [seconds]);
  return left;
}

export function ErrorView({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const { title, text } = describeApiError(error);
  const retryAfter =
    error instanceof ApiError && error.detail.kind === "http"
      ? error.detail.retryAfterSeconds
      : null;
  const left = useCountdown(error instanceof ApiError && error.status === 429 ? retryAfter : null);
  const canRetry =
    onRetry && !(error instanceof ApiError && [401, 403, 404].includes(error.status ?? 0));
  return (
    <div className="banner error" role="alert">
      <strong>{title}</strong>
      <p>{text}</p>
      {canRetry ? (
        <button type="button" onClick={onRetry} disabled={left !== null && left > 0}>
          {left !== null && left > 0 ? `Retry in ${left} s` : "Retry"}
        </button>
      ) : null}
    </div>
  );
}

/** Full-page states for route-level loads. */
export function QueryState({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (error instanceof ApiError && error.status === 403) return <ForbiddenPage />;
  if (error instanceof ApiError && error.status === 404) return <NotFoundPage />;
  return <ErrorView error={error} onRetry={onRetry} />;
}

export function ForbiddenPage() {
  return (
    <section aria-labelledby="forbidden-title">
      <h1 id="forbidden-title">Not allowed</h1>
      <p>Your role doesn't give access to this page. Ask the server owner if you need it.</p>
    </section>
  );
}

export function NotFoundPage() {
  return (
    <section aria-labelledby="notfound-title">
      <h1 id="notfound-title">Not found</h1>
      <p>This page or item doesn't exist. It may have been deleted, or the link is wrong.</p>
    </section>
  );
}

export function Loading({ label = "Loading…" }: { label?: string }) {
  return (
    <p role="status" className="muted">
      {label}
    </p>
  );
}
