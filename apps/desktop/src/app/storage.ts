// View preferences (layout, sort) kept in WebView storage. Only harmless UI state goes here: no
// secrets, nothing the Rust core needs. Storage can be unavailable, so every access is guarded and
// the default is used instead.
import { useCallback, useState } from "react";

function read<T>(key: string, fallback: T, valid: (value: unknown) => value is T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const value: unknown = JSON.parse(raw);
    return valid(value) ? value : fallback;
  } catch {
    return fallback;
  }
}

export function useStoredState<T>(
  key: string,
  fallback: T,
  valid: (value: unknown) => value is T,
): [T, (next: T) => void] {
  const [value, setValue] = useState<T>(() => read(key, fallback, valid));
  const set = useCallback(
    (next: T) => {
      setValue(next);
      try {
        localStorage.setItem(key, JSON.stringify(next));
      } catch {
        // Not persisted; the choice still applies for this session.
      }
    },
    [key],
  );
  return [value, set];
}

export function oneOf<T extends string>(...values: T[]): (value: unknown) => value is T {
  return (value: unknown): value is T => typeof value === "string" && values.includes(value as T);
}
