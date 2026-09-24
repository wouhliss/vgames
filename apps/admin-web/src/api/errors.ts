// Every way an API call can fail, as one typed error. Screens switch on `detail.kind`.
import { z } from "zod";

export const ProblemSchema = z.object({
  type: z.string(),
  title: z.string(),
  status: z.number().int().min(400).max(599),
  code: z.string().regex(/^[a-z][a-z0-9_]*$/),
  detail: z.string().optional(),
  instance: z.string().optional(),
  request_id: z.string().optional(),
  errors: z
    .array(z.object({ field: z.string(), code: z.string(), message: z.string().optional() }))
    .optional(),
});

/** RFC 9457 problem details (03-api §2). */
export type ApiProblem = z.infer<typeof ProblemSchema>;

export type ApiErrorDetail =
  /** The server answered with an error status. `problem` is synthesized when the body was not problem+json. */
  | {
      kind: "http";
      status: number;
      problem: ApiProblem;
      retryAfterSeconds: number | null;
      requestId: string | null;
    }
  /** No response: offline, DNS, connection refused, CORS. */
  | { kind: "network" }
  /** No response within the time limit. */
  | { kind: "timeout"; afterMs: number }
  /** The caller cancelled (navigation, unmount). Not shown to users. */
  | { kind: "aborted" }
  /** A 2xx response whose body was not valid JSON. */
  | { kind: "malformed"; status: number; requestId: string | null }
  /** A 2xx response that does not match the contract. */
  | { kind: "schema"; status: number; issues: string[]; requestId: string | null };

export class ApiError extends Error {
  readonly detail: ApiErrorDetail;

  constructor(detail: ApiErrorDetail) {
    super(
      detail.kind === "http"
        ? `${detail.status} ${detail.problem.code}`
        : detail.kind === "schema"
          ? `schema mismatch: ${detail.issues.slice(0, 3).join("; ")}`
          : detail.kind,
    );
    this.name = "ApiError";
    this.detail = detail;
  }

  get status(): number | null {
    return this.detail.kind === "http" ? this.detail.status : null;
  }

  get code(): string | null {
    return this.detail.kind === "http" ? this.detail.problem.code : null;
  }

  /** Worth retrying automatically: the request may succeed unchanged later. */
  get transient(): boolean {
    switch (this.detail.kind) {
      case "network":
      case "timeout":
        return true;
      case "http":
        return this.detail.status >= 500 || this.detail.status === 429;
      default:
        return false;
    }
  }

  /** Field errors from a `400 validation_failed`, keyed by dotted field path. */
  fieldErrors(): Record<string, string> {
    if (this.detail.kind !== "http") return {};
    const out: Record<string, string> = {};
    for (const e of this.detail.problem.errors ?? []) out[e.field] ??= e.message ?? e.code;
    return out;
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError;
}

export function hasStatus(error: unknown, status: number): boolean {
  return error instanceof ApiError && error.status === status;
}
