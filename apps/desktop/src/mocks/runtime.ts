// Shared pieces of the in-memory Rust core stand-in.

/** Thrown from a handler to make the command resolve to `{ status: "error", error }`. */
export class CommandFailure {
  constructor(readonly error: unknown) {}
}

export function fail(error: unknown): never {
  throw new CommandFailure(error);
}

export type Handler = (args: Record<string, unknown>) => unknown;

/** Deterministic PRNG (mulberry32), so fixtures are the same on every run. */
export function seeded(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let x = a;
    x = Math.imul(x ^ (x >>> 15), x | 1);
    x ^= x + Math.imul(x ^ (x >>> 7), x | 61);
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

/** A UUIDv7-looking id with a readable prefix and a counter. */
export function mockId(prefix: string, n: number): string {
  return `${prefix}-0000-7000-8000-${n.toString(16).padStart(12, "0")}`;
}

/** Runs `fn` after `ms`, or synchronously when `ms` is 0 (unit tests). */
export function later(ms: number, fn: () => void): void {
  if (ms <= 0) fn();
  else setTimeout(fn, ms);
}
