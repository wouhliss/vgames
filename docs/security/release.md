# Releasing the launcher

Owner: Agent 5. Workflows: [`release-desktop.yml`](../../.github/workflows/release-desktop.yml) (tag
`desktop-v*`) and [`desktop-matrix.yml`](../../.github/workflows/desktop-matrix.yml) (unsigned builds and
Rust tests on Windows, Linux and macOS for every relevant PR, nightly). Design: 08-release §1–§3.

Humans hold every key and approve every release; the workflow cannot publish anything on its own.

## One-time setup (humans)

1. **Environment `release`** (Settings → Environments): required reviewers (at least one person who is not
   the tagger), "Prevent self-review", deployment branches and tags limited to `desktop-v*`, `api-v*` and the
   `main` branch (the runtime catalog is signed after a merge to `main`). Every job that reads a secret runs in it.
2. **Tag ruleset**: `desktop-v*`, `api-v*` and `runtimes` (the runtime catalog's release) can be created only by
   maintainers and the release workflows, never deleted or moved.
3. **Updater key** (minisign), on an offline machine:
   ```sh
   pnpm --filter @vgames/desktop tauri signer generate -w vgames-updater.key
   ```
   Put the public key (the `.pub` file's content, a base64 string) in
   `apps/desktop/src-tauri/tauri.conf.json` → `plugins.updater.pubkey` through a normal PR. Store the private
   key file content as the secret `TAURI_SIGNING_PRIVATE_KEY` and its password as
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, both in the `release` environment only. Keep two offline copies:
   losing it means every launcher must be reinstalled by hand.
4. **Windows code signing**: `WINDOWS_CERTIFICATE` (base64 of the `.pfx`) and
   `WINDOWS_CERTIFICATE_PASSWORD`. Authenticode signs the launcher, its installer and every overlay binary
   (`vgames_overlay64.dll`, `vgames_overlay32.dll`, the 32-bit inject helper).
5. **Apple**: `APPLE_CERTIFICATE` (base64 `.p12` with the Developer ID Application certificate),
   `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific
   password), `APPLE_TEAM_ID`. The bundler signs and notarizes; the workflow checks `codesign`, the stapled
   ticket and Gatekeeper.
6. **`ANTHROPIC_API_KEY`** for the release-notes agent (08-release §3.4). Without it the linted player
   fragments are published verbatim.

## Why updater signatures name their version

`tauri signer sign` (Tauri CLI 2.11) writes `timestamp` and `file` in the signature's trusted comment, not
the version. `latest.json` itself is not signed, so a tampered `latest.json` could pair a higher version number
with an older, genuinely signed installer and push launchers back to a vulnerable build. The launcher
therefore sets `plugins.updater.requireSignedVersion: true`, and the release job re-signs every updater
artifact with `cargo xtask updater sign --version <V>` (trusted comment
`timestamp:…\tfile:…\tversion:<V>`). `cargo xtask updater manifest` refuses to write `latest.json` unless every
artifact verifies against the public key in `tauri.conf.json` and was signed for exactly this version.

## Cutting a release

1. A PR sets the version in `Cargo.toml` (`[workspace.package] version`) and `apps/desktop/package.json`.
2. After it merges, a maintainer pushes the tag `desktop-vX.Y.Z` on that commit.
3. Approve the `release` environment. The jobs:
   - **prepare**: tag = both versions; the updater key is set, `requireSignedVersion` and
     `createUpdaterArtifacts` are on.
   - **notes**: exports the fragments and the PRs merged since the previous tag, runs the release-notes agent,
     then `cargo xtask changelog release` (same guards again). Fails if the previous release's changelog PR was
     not merged.
   - **build** (Windows x64, Linux x64 on Ubuntu 22.04, macOS arm64): overlay binaries (Authenticode on
     Windows), `tauri build` with OS signing and notarization, updater artifacts re-signed for the version,
     OS signatures checked.
   - **publish**: `latest.json` (every artifact verified), `changelog-user.json`, the agent's output
     (`release-notes-agent.json`), `release-changelog.patch`, CycloneDX SBOMs (Rust and npm), `SHA256SUMS`,
     build provenance attestations, and a **draft** release.
4. Review the draft: the player notes (humans approve or reject, never edit them: 08-release §3), the
   checksums, and optionally the independent check below. Then publish it. Launchers pick it up from
   `releases/latest/download/latest.json` within 6 hours.
5. Open the changelog PR: `git apply release-changelog.patch` on a branch from `main` (it updates
   `CHANGELOG.md` and deletes the consumed fragments; the fragment gate accepts a release-assembly PR).

A rejected draft is deleted; fix, move to a new patch version, and tag again (tags are never moved).

## Verifying a release independently

```sh
gh release download desktop-vX.Y.Z --dir rel && cd rel
sha256sum -c SHA256SUMS
cargo xtask updater verify --latest latest.json --dir .          # minisign + signed version, every platform
gh attestation verify vgames_X.Y.Z_x64-setup.exe --repo wouhliss/vgames
```

## Releasing the server image

Workflow: [`release-api.yml`](../../.github/workflows/release-api.yml), tag `api-vX.Y.Z` (= the workspace
version). [`apps/api/Dockerfile`](../../apps/api/Dockerfile) builds the admin web and the API from source
into a distroless, non-root image (base images pinned by digest; runs with `--read-only`). The workflow builds
amd64 and arm64 natively without caches, pushes by digest, creates `ghcr.io/<owner>/vgames-api:X.Y.Z`,
signs it with cosign **keyless** (GitHub OIDC, no key to keep), and attaches a CycloneDX SBOM attestation and
build provenance. Verify before deploying:

```sh
cosign verify ghcr.io/wouhliss/vgames-api:X.Y.Z \
  --certificate-identity-regexp '^https://github.com/wouhliss/vgames/\.github/workflows/release-api\.yml@refs/tags/api-v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify oci://ghcr.io/wouhliss/vgames-api:X.Y.Z --repo wouhliss/vgames
```

Deploy by digest (`ghcr.io/…/vgames-api@sha256:…`), not by tag.

## Runtime catalog

[`runtimes/catalog.toml`](../../runtimes/catalog.toml) pins every compatibility runtime (09-compatibility §5).
`cargo xtask runtimes build` turns it into `runtimes.json` (`vgames.runtimes/1`) with the launcher's own
parser (`vgames_core::runtimes`), one version above the last published catalog, and refuses non-commercial
entries (D3DMetal) unless `commercial = false`. The launcher accepts only a catalog signed with the
**runtime-catalog key** (minisign, separate from the updater key) and never one older than it has seen.

**Key (humans, once, on an offline machine):**

```sh
minisign -G -p runtime-catalog.pub -s runtime-catalog.key     # choose a password
```

Commit `runtime-catalog.pub` as `runtimes/runtime-catalog.pub` through a normal PR (the launcher compiles it in).
Store the content of `runtime-catalog.key` as the secret `VGAMES_RUNTIME_CATALOG_KEY` and its password as
`VGAMES_RUNTIME_CATALOG_KEY_PASSWORD`, in the `release` environment only, and keep two offline copies.

**Repository setting (once):** Settings → Actions → General → Workflow permissions → "Allow GitHub Actions to
create and approve pull requests". Without it (or the optional App below), the watcher cannot open its PR.

**How a new runtime version gets to players:**

1. [`runtimes.yml`](../../.github/workflows/runtimes.yml) runs daily. For each upstream in
   [`runtimes/upstreams.toml`](../../runtimes/upstreams.toml) it finds the newest release and downloads the asset.
   It then checks the declared size, GitHub's SHA-256 digest and the upstream checksum file (GE-Proton and
   UMU-Proton `.sha512sum`), and the licenses (the repository's license must not change, and every SPDX id must be
   in `license_allowlist`).
2. It smoke-tests the new versions:
   - Linux: a D3D11 test program runs under umu-run and the new Proton in Xvfb, on Mesa's lavapipe (DXVK end to
     end).
   - macOS (Apple silicon runner, Rosetta): `wine --version`, a fresh prefix, and the same program. When the virtual
     machine has no GPU, the graphics check is reported as NOT VERIFIED rather than passed.
   - DXMT, DXVK-macOS and MoltenVK are not smoke-tested yet; their PR says so, so review them by hand.
3. It opens **one** PR from `runtimes/update` with the table of new versions and the smoke results. Later runs update
   that PR (or leave it alone when nothing changed) and never open a second one.
   - Integrity or license problems propose nothing, and the run fails.
   - Upstreams needing a human decision are listed under "Needs attention": a renamed asset, or a `repo_license` not
     yet recorded. Recording `repo_license` for each upstream is the one onboarding step.
4. Review the PR like code and merge it. [`release-runtimes.yml`](../../.github/workflows/release-runtimes.yml) then:
   1. verifies the published catalog;
   2. builds the next version;
   3. downloads every new entry again and checks it against its pin;
   4. after the `release` environment approval, signs it and verifies the signature with the committed public key;
   5. uploads `runtimes.json`, `runtimes.json.minisig` and a versioned copy to the `runtimes` release.
   That release is never marked latest, so the launcher's `releases/latest/download/latest.json` is unaffected.

**Optional: CI on the watcher's PRs.** A PR opened with the default `GITHUB_TOKEN` starts no workflows, so its required
checks never report. Create a GitHub App with *Contents* and *Pull requests* write access on this repository only.
Then create an environment `runtimes-bot` limited to `main`, and put the App's client id in the variable
`RUNTIMES_BOT_CLIENT_ID` and its private key in the secret `RUNTIMES_BOT_PRIVATE_KEY`. Without them, a maintainer
pushes a commit to `runtimes/update`, for example rebasing it on `main`, to start CI.

**Build, sign and check by hand** (what the release workflow does):

```sh
cargo xtask runtimes upsert new-entries.json                   # adds entries; a pinned version never changes bytes
cargo xtask runtimes build --previous last/runtimes.json --out runtimes.json
python3 scripts/runtimes/verify_new.py runtimes.json --previous last/runtimes.json
cargo xtask runtimes sign runtimes.json                        # reads the two variables above; writes runtimes.json.minisig
cargo xtask runtimes verify --last-version <published version> runtimes.json
```

## Scheduled workflows

| Workflow | When | What |
|---|---|---|
| [`e2e.yml`](../../.github/workflows/e2e.yml) | nightly, manual | The `vgames` CLI key pipeline against a real API (`scripts/e2e/key-pipeline.sh`), and the admin Playwright suite against the API serving the built admin web |
| [`desktop-matrix.yml`](../../.github/workflows/desktop-matrix.yml) | nightly, PRs touching the launcher crates | Unsigned builds and Rust tests on Windows, Linux and macOS |
| [`soak.yml`](../../.github/workflows/soak.yml) | weekly, manual | 24 h launcher leak test on a dedicated self-hosted runner (labels `self-hosted, linux, vgames-soak`); skipped until the repository variable `VGAMES_SOAK_RUNNER=true` |
| [`runtimes.yml`](../../.github/workflows/runtimes.yml) | daily, manual (`only`, `dry_run`) | The upstream runtime watcher above; downloads only when a new version appears |
| [Dependabot](../../.github/dependabot.yml) | weekly | Grouped updates; cryptography crates in their own PR; nothing is auto-merged |

## Dry run on a fork (A5-T07 acceptance)

1. Fork, then in the fork: create the `release` environment with a reviewer, generate a **test** updater key
   (step 3 above) and store it there, commit its public key to the fork's `tauri.conf.json`, and set the
   repository variable `VGAMES_UNSIGNED_DRY_RUN=true` (OS signing is skipped; the canonical repository
   ignores this variable and always signs).
2. Push a tag `desktop-v<version>` in the fork and approve the run.
3. Evidence to record in `docs/agents/status/agent-5.md`: the run link, `cargo xtask updater verify` output
   on the downloaded draft, an install of each artifact, and a launcher built with the test public key
   updating from the previous fork release.

Status: the pipeline is in place; the dry run needs a fork with a test key (humans), and the first real
release needs the secrets above.
