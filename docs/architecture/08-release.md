# 08 — CI/CD, Launcher Updates and the User-Facing Changelog

## 1. Pipelines (GitHub Actions; humans provide secrets and runners)

| Workflow | Trigger | Jobs |
|---|---|---|
| `ci.yml` | PR, push to `main` | Rust fmt, clippy (`-D warnings`), tests (Postgres service), sqlx offline check, `cargo deny`, `cargo audit`; TS Biome, typecheck, unit tests; OpenAPI lint + drift check; migration check on empty DB and upgrade; changelog fragment lint; desktop build check (Linux) |
| `desktop-matrix.yml` | PR touching `apps/desktop/**` or `crates/**`, nightly | Build the launcher on Windows x64, Linux x64, macOS arm64 (unsigned) + run Rust tests per OS |
| `e2e.yml` | nightly, manual | docker compose stack, API + admin-web Playwright suite, transfer round-trip tests (pack → upload → download) |
| `release-desktop.yml` | tag `desktop-v*` (protected) | Matrix build (includes the overlay binaries: `vgames_overlay64.dll`, `vgames_overlay32.dll` + 32-bit inject helper, `libvgames_overlay.so` + Vulkan layer manifests), OS code signing of every executable and DLL, updater signatures, `latest.json`, user changelog JSON (release-notes agent, §3.4), SBOM, provenance, draft GitHub Release. **Requires approval** of the `release` environment |
| `release-api.yml` | tag `api-v*` | Multi-arch container image (`ghcr.io/<owner>/vgames-api`, includes admin-web build), SBOM, provenance, signed with cosign keyless |
| `soak.yml` | weekly, manual | 24 h launcher leak test (RSS/handle counts) on a self-hosted runner |
| `runtimes.yml` | daily schedule, manual | Checks upstream releases of UMU-Proton, GE-Proton, umu-launcher, WineHQ macOS builds, DXMT, DXVK-macOS and MoltenVK; downloads, hashes, cross-checks upstream checksums, smoke-tests (Proton: run a D3D11 test exe headless under Xvfb; Wine: `wine --version` + a D3D11 test on a macOS runner) and **opens a PR** updating `runtimes/catalog.toml` |
| `release-runtimes.yml` | merge to `main` touching `runtimes/catalog.toml` | Builds `runtimes.json` from the catalog, signs it with the runtime-catalog key (`VGAMES_RUNTIME_CATALOG_KEY`), publishes it to the `runtimes` GitHub release. **Requires approval** of the `release` environment |

Rules: actions pinned by SHA; `permissions: {}` at the top of each workflow, widened per job;
no secrets in PR workflows from forks; caches keyed by lockfiles; `concurrency` cancels superseded runs.

## 2. Launcher auto-update

- `tauri-plugin-updater` with a **minisign** key pair (`pnpm tauri signer generate`).
  Public key in `tauri.conf.json` → `plugins.updater.pubkey`. Private key + password only in the
  `release` environment secrets (`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`).
- Endpoint: `https://github.com/<owner>/vgames/releases/latest/download/latest.json` (static, HTTPS).
- The Rust core checks at startup (after the UI is interactive), then every 6 h, and never while installing or playing.
  Verification: minisign signature over the artifact (plugin), semver strictly greater than the running version
  (no downgrade), HTTPS only. On Windows/macOS the OS signature is also checked by the installer.
- UX: a non-blocking "Update available" banner → **What's new** dialog (the changelog below) →
  **Install and restart** / **Later**. Installing waits for downloads to reach a checkpoint and
  refuses while a package is running.
- Channels: `stable` only in v1 (`latest.json`); the endpoint template leaves room for `beta`.

## 3. User-facing changelog (strict rule)

> The changelog shown in the launcher contains **only direct, user-facing changes**.
> Refactors, dependency bumps, CI, tests, internal tooling and performance work users
> cannot perceive never appear.

**The whole changelog is written and curated by agents. No human writes or edits changelog text.**
Two agent layers produce it, and deterministic checks guard both:

1. **The implementing agent** writes a fragment in the same PR as its change. It knows exactly what
   changed and whether a player can notice it (§3.1).
2. **The release-notes agent** (§3.4) runs in the release workflow. It re-reads every fragment of the
   release together with the merged PR titles, drops anything that is not a direct user-facing change,
   and rewrites the rest into consistent player-facing language.
3. **The lint** (§3.2) runs on both outputs. It cannot be overridden.

### 3.1 Fragments (every PR adds at least one, written by the agent that made the change)

`.changes/<short-slug>.md`:

```markdown
---
audience: user            # user | internal
component: launcher       # launcher | admin | server
type: added               # added | changed | fixed | removed | security
---
You can now pin favorite packages to the top of your library.
```

- `audience: internal` fragments satisfy the "every PR has a fragment" check and are
  **never** shown to users (they go only to `CHANGELOG.md`'s internal section).
- The launcher's changelog uses only `audience: user` **and** `component: launcher`.

### 3.2 Lint (`cargo xtask changelog lint`, CI on every PR)

For `audience: user` fragments:

- One or two sentences, 10–240 characters, written for players: starts with a capital letter, ends with a period.
- Rejects technical wording: a case-insensitive denylist (`refactor`, `dependency`, `dependencies`,
  `bump`, `upgrade to`, `crate`, `npm`, `pnpm`, `cargo`, `CI`, `pipeline`, `lint`, `clippy`,
  `test`, `tests`, `typescript`, `rust`, `tauri`, `react`, `sqlx`, `API`, `endpoint`, `schema`,
  `migration`, `internal`, `codebase`, `PR`, `commit`) plus patterns: backticks, file paths
  (`\w+/\w+`, `\.(rs|ts|tsx|json|yml|toml)`), issue/PR refs (`#\d+`), hashes (`[0-9a-f]{7,}`).
- `type: security` fragments must describe the user impact ("Fixed an issue that could …") and never exploit details.
- A PR that changes `apps/desktop/**` without a `component: launcher` fragment gets a warning
  asking the author to confirm `audience: internal` is right.

### 3.3 Release assembly (`cargo xtask changelog release <version>`)

Runs after the release-notes agent (§3.4) and consumes its validated output for the user-facing files.

1. Collect fragments since the last release; delete them in the release commit.
2. Prepend `CHANGELOG.md` (all audiences, grouped by component and type).
3. Emit `changelog-user.json` (release asset), cumulative across releases:

```json
{ "format": "vgames.changelog/1",
  "releases": [
    { "version": "0.4.0", "date": "2026-10-02",
      "entries": [ { "type": "added", "text": "You can now pin favorite packages to the top of your library." } ] } ] }
```

4. Put the same entries for this version into `latest.json` `notes` (plain text, one bullet per line).

The launcher's **What's new** dialog shows every release in `(installed, new]` from
`changelog-user.json` (fetched over HTTPS; if unavailable, `notes` from `latest.json`), grouped by type
("New", "Improved", "Fixed", "Removed", "Security"), rendered as **plain text**.
A release with zero user entries shows "Stability and performance improvements." and nothing else.

### 3.4 Release-notes agent (release workflow)

A small TypeScript script (`scripts/release-notes/`, official `@anthropic-ai/sdk`) runs in
`release-desktop.yml` before assembly.

- **Input:** every `.changes/*.md` fragment in the release (both audiences) and the titles and bodies of the
  merged PRs since the previous tag. Nothing else: no diffs, and no secrets in the prompt.
- **Model and call:** `claude-opus-5` with adaptive thinking and **structured outputs**
  (`client.messages.parse` + `zodOutputFormat`) against this schema:
  `{ entries: [{ type: added|changed|fixed|removed|security, text, source_fragments: [slug] }],
  dropped: [{ slug, reason }] }`. It handles `stop_reason: "refusal"` with server-side fallbacks
  (`fallbacks: "default"`, beta `server-side-fallback-2026-07-01`) and fails the job on any other
  non-`end_turn` stop.
- **Instructions (system prompt, versioned in the repo):** keep only changes a player can see or
  feel. Merge duplicates. Write one plain sentence per entry. Never invent a change that has no source
  fragment. Every `entries[].source_fragments` must name existing `audience: user` fragments.
- **Guardrails (deterministic, in the same job):**
  - Schema validation and the §3.2 lint on every entry. On failure the agent gets one retry with the lint
    errors, then the job fails.
  - Entries can only come *from* user fragments: an entry citing an `internal` fragment is rejected.
    The agent may drop or reword, never promote.
  - The output is attached to the draft release for the humans who approve the `release` environment,
    who approve or reject but never edit the text.
- **Secret:** `ANTHROPIC_API_KEY` exists only in the `release` environment (infra/README.md).
- Deterministic fallback when the API is unavailable: publish the linted user fragments verbatim.
