import type { Fragment, Notes } from "./schema.ts";

/** Lints one player-facing text; returns the problems (empty = valid). */
export type Lint = (text: string, type: string) => Promise<string[]>;

/**
 * The deterministic guards run on the model's output (08-release §3.4). The
 * agent may drop or reword user fragments but never promote an internal one:
 * every entry must cite only existing `audience: user` + `component: launcher`
 * fragments, and every text must pass the same lint as the fragments.
 */
export async function checkNotes(
  notes: Notes,
  fragments: Fragment[],
  lint: Lint,
): Promise<string[]> {
  const bySlug = new Map(fragments.map((f) => [f.slug, f]));
  const problems: string[] = [];
  for (const [i, e] of notes.entries.entries()) {
    const where = `entries[${i}] (${JSON.stringify(e.text.slice(0, 60))})`;
    if (e.source_fragments.length === 0) {
      problems.push(`${where}: cites no fragment`);
    }
    for (const slug of e.source_fragments) {
      const f = bySlug.get(slug);
      if (!f) {
        problems.push(`${where}: cites unknown fragment ${JSON.stringify(slug)}`);
      } else if (f.audience !== "user") {
        problems.push(
          `${where}: cites internal fragment ${JSON.stringify(slug)}; internal changes never reach players`,
        );
      } else if (f.component !== "launcher") {
        problems.push(
          `${where}: cites ${f.component} fragment ${JSON.stringify(slug)}; only launcher changes belong here`,
        );
      }
    }
    for (const p of await lint(e.text, e.type)) {
      problems.push(`${where}: ${p}`);
    }
  }
  return problems;
}

/** Used when the API is unavailable: the linted user launcher fragments, verbatim. */
export function deterministicNotes(fragments: Fragment[]): Notes {
  const used = fragments.filter((f) => f.audience === "user" && f.component === "launcher");
  return {
    entries: used.map((f) => ({ type: f.type, text: f.text, source_fragments: [f.slug] })),
    dropped: fragments
      .filter((f) => !used.includes(f))
      .map((f) => ({ slug: f.slug, reason: `${f.audience} ${f.component} fragment` })),
  };
}
