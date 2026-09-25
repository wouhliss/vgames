# Release-notes agent

Turns the release's changelog fragments and merged PR titles into the player-facing "What's new"
(08-release §3.4). Owner: Agent 5. Runs in `release-desktop.yml`, in the `release` environment (the only
place `ANTHROPIC_API_KEY` exists).

```sh
cargo xtask changelog export > fragments.json          # every fragment, both audiences
# prs.json: [{ "title": "...", "body": "..." }] of the PRs merged since the previous tag
node --experimental-strip-types scripts/release-notes/src/main.ts \
  --fragments fragments.json --prs prs.json --out notes.json
cargo xtask changelog release 0.4.0 --notes notes.json --date 2026-10-02 \
  --previous-user-json previous/changelog-user.json --out-dir dist
```

- Model `claude-opus-5`, adaptive thinking, structured output (`client.beta.messages.parse` +
  `betaZodOutputFormat`), server-side refusal fallbacks (`fallbacks: "default"`, beta
  `server-side-fallback-2026-07-01`). A refusal or any stop other than `end_turn` fails the job.
- System prompt: [`prompts/system.md`](prompts/system.md) (versioned here).
- Guards on every answer: schema; entries may cite only `audience: user` + `component: launcher` fragments;
  every text passes `cargo xtask changelog lint-text`. One retry with the problems, then the job fails.
  `cargo xtask changelog release` runs the same guards again.
- API unavailable (or no key): the linted user launcher fragments are published verbatim.
- Tests replay fixed responses (`test/golden.test.ts`); no live API in CI.
