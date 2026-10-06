// Unsaved form input that survives a sign-in round trip (A3-T18).
//
// When the session ends mid-use (any 401), the app leaves for the sign-in page and then Discord, so
// every in-memory form is lost. Just before that, `stashDrafts()` asks each mounted form for its
// unsaved input and writes it to sessionStorage (this tab only). After signing in again, the form
// takes its draft back once, when it mounts, and says so.
//
// - Drafts belong to the account that typed them: another account signing in on this tab never
//   sees them.
// - They keep the version (ETag) they were edited against, so a change someone else saved meanwhile
//   still ends in the usual conflict screen instead of being overwritten.
// - They expire after a day and are validated on the way back in.
// - Never secrets: passphrases, key files and upload files are not drafts.
import { useEffect, useRef, useState } from "react";
import type { z } from "zod";
import { useMe } from "./session";

const PREFIX = "vgames.admin.draft:";
const MAX_AGE_MS = 24 * 60 * 60 * 1000;

interface Stored {
  user: string;
  at: number;
  value: unknown;
}

/** Mounted forms: draft key → current unsaved input (`null` when there is none). */
const live = new Map<string, { user: string; snapshot: () => unknown }>();
let signingOut = false;

function storage(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

/** Writes every mounted form's unsaved input. Called right before leaving for sign-in. */
export function stashDrafts(): void {
  signingOut = true;
  const store = storage();
  if (!store) return;
  for (const [key, entry] of live) {
    const value = entry.snapshot();
    if (value === null) continue;
    const stored: Stored = { user: entry.user, at: Date.now(), value };
    try {
      store.setItem(PREFIX + key, JSON.stringify(stored));
    } catch {
      // Storage full or blocked: the form is lost, as it would have been without drafts.
    }
  }
}

/** True once the app is leaving for sign-in: "unsaved changes" prompts must not stop it. */
export function isSigningOut(): boolean {
  return signingOut;
}

/** The session turned out to be fine (signed in without leaving the app): prompts apply again. */
export function signedInAgain(): void {
  signingOut = false;
}

/** For tests: forget the sign-out and every stored draft. */
export function resetDrafts(): void {
  signingOut = false;
  live.clear();
  const store = storage();
  if (!store) return;
  for (let i = store.length - 1; i >= 0; i--) {
    const key = store.key(i);
    if (key?.startsWith(PREFIX)) store.removeItem(key);
  }
}

function take<T>(key: string, user: string, schema: z.ZodType<T>): T | null {
  const store = storage();
  if (!store) return null;
  const raw = store.getItem(PREFIX + key);
  if (raw === null) return null;
  store.removeItem(PREFIX + key);
  try {
    const stored = JSON.parse(raw) as Partial<Stored>;
    if (stored.user !== user || typeof stored.at !== "number") return null;
    if (Date.now() - stored.at > MAX_AGE_MS) return null;
    const parsed = schema.safeParse(stored.value);
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

/**
 * Registers a form's unsaved input under `key` while it is mounted, and returns the draft saved for
 * it before the last sign-in (once; `null` when there is none).
 *
 *     const restored = useDraft(`package:${id}`, DraftSchema, () => (dirty ? { etag, values } : null));
 */
export function useDraft<T>(key: string, schema: z.ZodType<T>, snapshot: () => T | null): T | null {
  const user = useMe().data?.user.id ?? "";
  const [restored] = useState(() => (user ? take(key, user, schema) : null));
  const latest = useRef(snapshot);
  latest.current = snapshot;
  useEffect(() => {
    if (!user) return;
    const entry = { user, snapshot: () => latest.current() };
    live.set(key, entry);
    return () => {
      if (live.get(key) === entry) live.delete(key);
    };
  }, [key, user]);
  return restored;
}

export const RESTORED_MESSAGE =
  "Restored the changes you hadn't saved before signing in again. Review them, then save.";
