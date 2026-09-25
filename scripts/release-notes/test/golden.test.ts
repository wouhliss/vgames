import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import Anthropic from "@anthropic-ai/sdk";
import { describe, expect, it } from "vitest";

import { curate, type Parse, type ParsedResponse, ReleaseNotesError } from "../src/agent.ts";
import type { Lint } from "../src/guards.ts";
import type { Fragment } from "../src/schema.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const load = <T>(name: string): T => JSON.parse(readFileSync(path.join(here, "fixtures", name), "utf8")) as T;
const fragments = load<Fragment[]>("fragments.json");
const system = readFileSync(path.join(here, "../prompts/system.md"), "utf8");

/** A stand-in for `cargo xtask changelog lint-text` covering the rules these fixtures hit. */
const lint: Lint = async (text) =>
  /\b(refactor\w*|dependenc\w*|bump\w*|crate|tauri)\b/i.test(text) || !text.endsWith(".")
    ? ["uses technical wording or is not a sentence"]
    : [];

/** Replays recorded responses in order and records the requests. */
function replay(...responses: ParsedResponse[]): { parse: Parse; requests: Parameters<Parse>[0][] } {
  const requests: Parameters<Parse>[0][] = [];
  const queue = [...responses];
  return {
    requests,
    parse: async (req) => {
      requests.push(structuredClone(req));
      const next = queue.shift();
      if (!next) throw new Error("no more recorded responses");
      return next;
    },
  };
}

describe("release-notes agent", () => {
  it("rejects an entry promoted from an internal fragment, then accepts the corrected retry", async () => {
    const { parse, requests } = replay(
      load("attempt1-promotes-internal.json"),
      load("attempt2-corrected.json"),
    );
    const notes = await curate({ fragments, prs: [], system, parse, lint });
    expect(notes.source).toBe("agent");
    expect(notes.entries.map((e) => e.source_fragments)).toEqual([["resume-after-restart"], ["pin-favorites"]]);
    // The retry told the model exactly why.
    expect(requests).toHaveLength(2);
    const feedback = JSON.stringify(requests[1]?.messages.at(-1));
    expect(feedback).toContain("cites internal fragment");
    expect(feedback).toContain("download-engine-refactor");
  });

  it("fails the job when the guards reject the answer twice", async () => {
    const bad = load<ParsedResponse>("attempt1-promotes-internal.json");
    const { parse } = replay(bad, bad);
    await expect(curate({ fragments, prs: [], system, parse, lint })).rejects.toThrow(/failed the guards twice/);
  });

  it("refuses entries citing admin fragments or unknown slugs", async () => {
    const answer: ParsedResponse = {
      model: "claude-opus-5",
      stop_reason: "end_turn",
      parsed_output: {
        entries: [
          { type: "added", text: "Admins can now withdraw several versions at once.", source_fragments: ["admin-bulk-yank"] },
          { type: "added", text: "Something that never happened is here.", source_fragments: ["made-up"] },
        ],
        dropped: [],
      },
    };
    const { parse } = replay(answer, answer);
    await expect(curate({ fragments, prs: [], system, parse, lint })).rejects.toThrow(/admin fragment[\s\S]*unknown fragment/);
  });

  it("fails on a refusal and on any stop reason other than end_turn", async () => {
    for (const stop_reason of ["refusal", "max_tokens"]) {
      const { parse } = replay({ model: "claude-opus-5", stop_reason, parsed_output: null });
      await expect(curate({ fragments, prs: [], system, parse, lint })).rejects.toBeInstanceOf(ReleaseNotesError);
    }
  });

  it("publishes the user launcher fragments verbatim when the API is unavailable", async () => {
    const parse: Parse = async () => {
      throw new Anthropic.APIConnectionError({ message: "connection refused" });
    };
    const notes = await curate({ fragments, prs: [], system, parse, lint });
    expect(notes.source).toBe("fallback");
    expect(notes.entries.map((e) => e.text)).toEqual([
      "Downloads now resume after your computer restarts.",
      "You can now pin favorite packages to the top of your library.",
    ]);
    // Without credentials, same result.
    expect((await curate({ fragments, prs: [], system, parse: null, lint })).entries).toHaveLength(2);
  });

  it("an internal-only release has no entries", async () => {
    const internal = fragments.filter((f) => f.audience === "internal");
    const { parse } = replay({
      model: "claude-opus-5",
      stop_reason: "end_turn",
      parsed_output: { entries: [], dropped: [{ slug: "download-engine-refactor", reason: "internal" }] },
    });
    expect((await curate({ fragments: internal, prs: [], system, parse, lint })).entries).toEqual([]);
    expect((await curate({ fragments: internal, prs: [], system, parse: null, lint })).entries).toEqual([]);
  });
});
