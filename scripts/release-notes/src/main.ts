/**
 * Release-notes agent (08-release §3.4). Runs in release-desktop.yml, in the
 * `release` environment (the only place ANTHROPIC_API_KEY exists):
 *
 *   cargo xtask changelog export > fragments.json
 *   node --experimental-strip-types scripts/release-notes/src/main.ts \
 *     --fragments fragments.json --prs prs.json --out notes.json
 *
 * Input is only the fragments and merged PR titles/bodies; no diffs, no secrets.
 */
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import Anthropic from "@anthropic-ai/sdk";
import { z } from "zod";

import { apiParse, curate } from "./agent.ts";
import type { Lint } from "./guards.ts";
import { FragmentSchema, PullRequestSchema } from "./schema.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../../..");

/** The one implementation of the §3.2 lint: `cargo xtask changelog lint-text`. */
export const xtaskLint: Lint = async (text, type) => {
  try {
    // Synchronous on purpose: only the sync variant pipes `input` to stdin and closes it.
    execFileSync("cargo", ["xtask", "changelog", "lint-text", "--type", type], {
      cwd: repo,
      input: text,
      stdio: ["pipe", "pipe", "pipe"],
      timeout: 300_000,
    });
    return [];
  } catch (err) {
    const stdout = String((err as { stdout?: Buffer | string }).stdout ?? "");
    const lines = stdout.split("\n").filter((l) => l.trim() !== "");
    return lines.length > 0 ? lines : [`lint failed to run: ${String(err)}`];
  }
};

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      fragments: { type: "string" },
      prs: { type: "string" },
      out: { type: "string" },
    },
  });
  if (!values.fragments || !values.out) {
    throw new Error("usage: main.ts --fragments fragments.json [--prs prs.json] --out notes.json");
  }
  const fragments = z
    .array(FragmentSchema)
    .parse(JSON.parse(readFileSync(values.fragments, "utf8")));
  const prs = values.prs
    ? z.array(PullRequestSchema).parse(JSON.parse(readFileSync(values.prs, "utf8")))
    : [];
  const system = readFileSync(path.join(here, "../prompts/system.md"), "utf8");
  const parse = process.env.ANTHROPIC_API_KEY ? apiParse(new Anthropic()) : null;
  const notes = await curate({
    fragments,
    prs,
    system,
    parse,
    lint: xtaskLint,
    log: console.error,
  });
  writeFileSync(values.out, `${JSON.stringify(notes, null, 2)}\n`);
  console.error(
    `release notes: ${notes.entries.length} entries (${notes.source}), ${notes.dropped.length} dropped`,
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((err: unknown) => {
    console.error(err instanceof Error ? err.message : err);
    process.exit(1);
  });
}
