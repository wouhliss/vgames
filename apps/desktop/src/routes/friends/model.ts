// Pure helpers for the social screens: names, presence, ordering, friend codes and error text.
import { formatDuration, t } from "../../i18n";
import type { Friend, Presence, SocialError, UserSummary } from "../../ipc";

export const MESSAGE_MAX = 4000;
export const INVITE_MESSAGE_MAX = 200;
export const CODE_LENGTH = 8;

export const displayName = (user: UserSummary): string => user.display_name ?? user.username;

export type PresenceGroup = "playing" | "online" | "offline";

export function presenceGroup(presence: Presence | null): PresenceGroup {
  switch (presence?.status) {
    case "in_game":
      return "playing";
    case "online":
    case "away":
      return "online";
    default:
      return "offline";
  }
}

export function presenceText(presence: Presence | null): string {
  switch (presence?.status) {
    case "in_game":
      return presence.package_title
        ? t("friends.presence.playing", { game: presence.package_title })
        : t("friends.presence.playingHidden");
    case "online":
      return t("friends.presence.online");
    case "away":
      return t("friends.presence.away");
    default:
      return t("friends.presence.offline");
  }
}

const GROUP_ORDER: Record<PresenceGroup, number> = { playing: 0, online: 1, offline: 2 };
const collator = new Intl.Collator(undefined, { sensitivity: "base" });

/** Playing, then online (online before away), then offline; by name inside a group. */
export function sortFriends(friends: readonly Friend[]): Friend[] {
  return [...friends].sort((a, b) => {
    const group = GROUP_ORDER[presenceGroup(a.presence)] - GROUP_ORDER[presenceGroup(b.presence)];
    if (group !== 0) return group;
    const away = Number(a.presence?.status === "away") - Number(b.presence?.status === "away");
    if (away !== 0) return away;
    return collator.compare(displayName(a.user), displayName(b.user));
  });
}

/**
 * Friend codes are Crockford base32 (05-social-notes §1): upper-case, O → 0, I and L → 1, U is not
 * used, and dashes or spaces are ignored. Anything else is dropped. At most 8 characters.
 */
export function normalizeFriendCode(input: string): string {
  let out = "";
  for (const raw of input.toUpperCase()) {
    const c = raw === "O" ? "0" : raw === "I" || raw === "L" ? "1" : raw;
    if (/^[0-9A-HJKMNP-TV-Z]$/.test(c)) out += c;
    if (out.length === CODE_LENGTH) break;
  }
  return out;
}

export const isCompleteCode = (code: string) => /^[0-9A-HJKMNP-TV-Z]{8}$/.test(code);

/** "Q4TR8WZN" → "Q4TR 8WZN" (easier to read aloud). */
export const groupCode = (code: string) => `${code.slice(0, 4)} ${code.slice(4)}`;

/** Seconds left until `iso`, never negative. */
export function secondsUntil(iso: string, now: number): number {
  const at = Date.parse(iso);
  return Number.isNaN(at) ? 0 : Math.max(0, Math.ceil((at - now) / 1000));
}

/** "14:05" for a countdown. */
export function clock(totalSeconds: number): string {
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** The launcher's own join-secret rule (05-social §5). */
export const JOIN_SECRET = /^[A-Za-z0-9._:\-[\]]{1,256}$/;

/** A plain sentence for every `SocialError`. */
export function socialErrorText(error: SocialError | null): string {
  switch (error?.kind) {
    case "code_invalid":
      return t("friends.error.codeInvalid");
    case "invalid_input":
      return error.field === "code" ? t("friends.error.invalidCode") : t("friends.error.generic");
    case "rate_limited":
      return t("friends.error.rateLimited", {
        time: formatDuration(Math.max(1, error.retry_after_seconds)),
      });
    case "limit_reached":
      return error.limit === "friends"
        ? t("friends.error.limitFriends")
        : error.limit === "pending_requests"
          ? t("friends.error.limitRequests")
          : t("friends.error.limitParty");
    case "conflict":
      return error.code === "already_friends"
        ? t("friends.error.alreadyFriends")
        : t("friends.error.conflict");
    case "not_found":
      return t("friends.error.notFound");
    case "offline":
      return t("friends.error.offline");
    case "not_signed_in":
      return t("friends.error.signedOut");
    default:
      return t("friends.error.generic");
  }
}
