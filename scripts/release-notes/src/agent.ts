import Anthropic from "@anthropic-ai/sdk";
import { betaZodOutputFormat } from "@anthropic-ai/sdk/helpers/beta/zod";

import { checkNotes, deterministicNotes, type Lint } from "./guards.ts";
import { type Fragment, type Notes, NotesSchema, type PullRequest, type ReleaseNotes } from "./schema.ts";

export const MODEL = "claude-opus-5";
/** `fallbacks: "default"` (routes declined requests by refusal category) needs this beta header. */
export const FALLBACK_BETA = "server-side-fallback-2026-07-01";

/** The part of a parsed response the agent reads. */
export type ParsedResponse = {
  model: string;
  stop_reason: string | null;
  parsed_output: Notes | null;
};

/** `client.beta.messages.parse`, injectable so tests can replay recorded responses. */
export type Parse = (params: {
  system: string;
  messages: Anthropic.Beta.BetaMessageParam[];
}) => Promise<ParsedResponse>;

export class ReleaseNotesError extends Error {}

export function apiParse(client: Anthropic): Parse {
  return async ({ system, messages }) => {
    const res = await client.beta.messages.parse({
      model: MODEL,
      max_tokens: 16000,
      betas: [FALLBACK_BETA],
      fallbacks: "default",
      thinking: { type: "adaptive" },
      system,
      messages,
      output_config: { format: betaZodOutputFormat(NotesSchema) },
    });
    return { model: res.model, stop_reason: res.stop_reason, parsed_output: res.parsed_output };
  };
}

/** Errors meaning the API could not be reached or used, as opposed to a bad answer. */
export function isUnavailable(err: unknown): boolean {
  return (
    err instanceof Anthropic.APIConnectionError ||
    err instanceof Anthropic.RateLimitError ||
    err instanceof Anthropic.InternalServerError ||
    err instanceof Anthropic.AuthenticationError ||
    (err instanceof Anthropic.APIError && (err.status ?? 0) >= 500)
  );
}

function prompt(fragments: Fragment[], prs: PullRequest[]): string {
  return [
    "Changelog fragments of this release (JSON):",
    JSON.stringify(fragments, null, 2),
    "",
    "Pull requests merged since the previous release, for context only (JSON):",
    JSON.stringify(prs, null, 2),
  ].join("\n");
}

async function ask(parse: Parse, system: string, messages: Anthropic.Beta.BetaMessageParam[]): Promise<ParsedResponse & { parsed_output: Notes }> {
  const res = await parse({ system, messages });
  // Check why the model stopped before reading anything it produced.
  if (res.stop_reason === "refusal") {
    throw new ReleaseNotesError("the model declined, including the server-side fallback models");
  }
  if (res.stop_reason !== "end_turn") {
    throw new ReleaseNotesError(`the model stopped with ${res.stop_reason ?? "no stop reason"}`);
  }
  if (res.parsed_output === null) {
    throw new ReleaseNotesError("the response does not match the notes schema");
  }
  return { ...res, parsed_output: res.parsed_output };
}

/**
 * Curates the release's fragments into player-facing notes. The guards run on
 * every answer; one retry gets the problems, a second failure fails the job.
 * If the API is unavailable, the linted user launcher fragments are used verbatim.
 */
export async function curate(opts: {
  fragments: Fragment[];
  prs: PullRequest[];
  system: string;
  parse: Parse | null;
  lint: Lint;
  log?: (line: string) => void;
}): Promise<ReleaseNotes> {
  const log = opts.log ?? (() => {});
  const fallback = async (why: string): Promise<ReleaseNotes> => {
    log(`release notes: ${why}; publishing the user launcher fragments verbatim`);
    const notes = deterministicNotes(opts.fragments);
    const problems = await checkNotes(notes, opts.fragments, opts.lint);
    if (problems.length > 0) {
      throw new ReleaseNotesError(`fragments fail the lint:\n${problems.join("\n")}`);
    }
    return { source: "fallback", model: null, ...notes };
  };
  if (opts.parse === null) {
    return fallback("no API credentials");
  }
  const messages: Anthropic.Beta.BetaMessageParam[] = [
    { role: "user", content: prompt(opts.fragments, opts.prs) },
  ];
  for (let attempt = 1; attempt <= 2; attempt++) {
    let res: ParsedResponse & { parsed_output: Notes };
    try {
      res = await ask(opts.parse, opts.system, messages);
    } catch (err) {
      if (isUnavailable(err)) {
        return fallback(`the API is unavailable (${err instanceof Error ? err.message : String(err)})`);
      }
      throw err;
    }
    const problems = await checkNotes(res.parsed_output, opts.fragments, opts.lint);
    if (problems.length === 0) {
      return { source: "agent", model: res.model, ...res.parsed_output };
    }
    log(`release notes: attempt ${attempt} rejected:\n${problems.join("\n")}`);
    if (attempt === 2) {
      throw new ReleaseNotesError(`the notes failed the guards twice:\n${problems.join("\n")}`);
    }
    messages.push(
      { role: "assistant", content: JSON.stringify(res.parsed_output) },
      {
        role: "user",
        content: `These notes were rejected by the release checks:\n${problems.join("\n")}\n\nReturn corrected notes. Remember: cite only user launcher fragments, and write plain sentences for players.`,
      },
    );
  }
  throw new ReleaseNotesError("unreachable");
}
