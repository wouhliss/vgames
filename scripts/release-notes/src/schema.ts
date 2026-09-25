import { z } from "zod";

export const CHANGE_TYPES = ["added", "changed", "fixed", "removed", "security"] as const;

/** The model's structured output (08-release §3.4). */
export const NotesSchema = z.object({
  entries: z.array(
    z.object({
      type: z.enum(CHANGE_TYPES),
      text: z.string(),
      source_fragments: z.array(z.string()),
    }),
  ),
  dropped: z.array(z.object({ slug: z.string(), reason: z.string() })),
});
export type Notes = z.infer<typeof NotesSchema>;

/** One fragment, as exported by `cargo xtask changelog export`. */
export const FragmentSchema = z.object({
  slug: z.string(),
  audience: z.enum(["user", "internal"]),
  component: z.enum(["launcher", "admin", "server"]),
  type: z.enum(CHANGE_TYPES),
  text: z.string(),
});
export type Fragment = z.infer<typeof FragmentSchema>;

export const PullRequestSchema = z.object({ title: z.string(), body: z.string().nullable() });
export type PullRequest = z.infer<typeof PullRequestSchema>;

/** What the release job hands to `cargo xtask changelog release`. */
export type ReleaseNotes = {
  source: "agent" | "fallback";
  model: string | null;
  entries: Notes["entries"];
  dropped: Notes["dropped"];
};
