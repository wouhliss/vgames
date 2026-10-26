// Failure diagnostics retain only fixed state/error kinds, never job metadata or messages.
const object = (value: unknown): Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};

export function summarizeDownloads(value: unknown) {
  const queue = object(value);
  if (!Array.isArray(queue.jobs) || !Array.isArray(queue.history)) return { available: false };
  const states = queue.jobs.map((job: unknown) => object(object(job).state));
  const outcomes = queue.history.map((entry: unknown) => object(object(entry).outcome));
  const count = (rows: Record<string, unknown>[], kind: string) =>
    rows.filter((row) => row.kind === kind).length;
  const errors = states.filter((row) => row.kind === "failed").map((row) => object(row.error));
  const pauses = states.filter((row) => row.kind === "paused").map((row) => object(row.reason));
  return {
    available: true,
    queued: count(states, "queued"),
    active: count(states, "active"),
    paused: count(states, "paused"),
    failed: count(states, "failed"),
    installed: count(outcomes, "installed"),
    history_failed: count(outcomes, "failed"),
    cancelled: count(outcomes, "cancelled"),
    pauses: {
      user: count(pauses, "user"),
      disk_full: count(pauses, "disk_full"),
      library_offline: count(pauses, "library_offline"),
      offline: count(pauses, "offline"),
    },
    errors: {
      damaged_file: count(errors, "damaged_file"),
      signature_invalid: count(errors, "signature_invalid"),
      untrusted_key: count(errors, "untrusted_key"),
      trust_expired: count(errors, "trust_expired"),
      version_unavailable: count(errors, "version_unavailable"),
      io: count(errors, "io"),
      server: count(errors, "server"),
      blocked: count(errors, "blocked"),
    },
  };
}
