import { describe, expect, it } from "vitest";
import type { Friend, Presence } from "../../ipc";
import {
  clock,
  groupCode,
  isCompleteCode,
  JOIN_SECRET,
  normalizeFriendCode,
  presenceText,
  secondsUntil,
  socialErrorText,
  sortFriends,
} from "./model";

const presence = (status: Presence["status"], title: string | null = null): Presence => ({
  status,
  package_id: title ? "p" : null,
  package_title: title,
  updated_at: null,
});

const friend = (name: string, p: Presence | null): Friend => ({
  user: { id: name, username: name.toLowerCase(), display_name: name, avatar_url: null },
  state: "accepted",
  presence: p,
  since: null,
});

describe("normalizeFriendCode", () => {
  it("applies the Crockford decoding rules", () => {
    expect(normalizeFriendCode("q4tr-8wzn")).toBe("Q4TR8WZN");
    expect(normalizeFriendCode("oIl0 ab")).toBe("0110AB");
    expect(normalizeFriendCode("U u!?")).toBe("");
    expect(normalizeFriendCode("ABCDEFGH123")).toBe("ABCDEFGH");
  });

  it("recognises a complete code", () => {
    expect(isCompleteCode("Q4TR8WZN")).toBe(true);
    expect(isCompleteCode("Q4TR8WZ")).toBe(false);
    expect(groupCode("Q4TR8WZN")).toBe("Q4TR 8WZN");
  });
});

describe("presence", () => {
  it("names the game only when it is shared", () => {
    expect(presenceText(presence("in_game", "Hollow Harbor"))).toBe("Playing Hollow Harbor");
    expect(presenceText(presence("in_game"))).toBe("Playing a game");
    expect(presenceText(null)).toBe("Offline");
  });

  it("sorts playing, online, away, offline, then by name", () => {
    const sorted = sortFriends([
      friend("Zed", presence("offline")),
      friend("Amy", presence("away")),
      friend("Bob", presence("online")),
      friend("Cat", presence("in_game", "X")),
      friend("Abe", null),
    ]).map((f) => f.user.display_name);
    expect(sorted).toEqual(["Cat", "Bob", "Amy", "Abe", "Zed"]);
  });
});

describe("countdown", () => {
  it("counts down in minutes and seconds and stops at zero", () => {
    const now = Date.parse("2026-09-28T10:00:00Z");
    expect(secondsUntil("2026-09-28T10:15:00Z", now)).toBe(900);
    expect(secondsUntil("2026-09-28T09:00:00Z", now)).toBe(0);
    expect(clock(900)).toBe("15:00");
    expect(clock(65)).toBe("1:05");
  });
});

describe("join secret", () => {
  it("accepts addresses and lobby codes only", () => {
    expect(JOIN_SECRET.test("192.168.1.20:27015")).toBe(true);
    expect(JOIN_SECRET.test("[::1]:7777")).toBe(true);
    expect(JOIN_SECRET.test("lobby code")).toBe(false);
    expect(JOIN_SECRET.test("a;rm -rf")).toBe(false);
    expect(JOIN_SECRET.test("x".repeat(257))).toBe(false);
  });
});

describe("socialErrorText", () => {
  it("has a sentence for every error", () => {
    expect(socialErrorText({ kind: "code_invalid" })).toMatch(/expired or already been used/);
    expect(socialErrorText({ kind: "rate_limited", retry_after_seconds: 120 })).toBe(
      "Too many tries. Try again in 2 minutes.",
    );
    expect(socialErrorText({ kind: "limit_reached", limit: "friends" })).toMatch(/maximum/);
    expect(socialErrorText({ kind: "conflict", code: "already_friends", message: "" })).toMatch(
      /already friends/,
    );
    expect(socialErrorText({ kind: "internal", detail: "x" })).toBe(
      "Something went wrong. Try again.",
    );
  });
});
