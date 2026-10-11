# Releasing the launcher

Owner: INT. Workflows: [`release-desktop.yml`](../../.github/workflows/release-desktop.yml) (tag
`desktop-v*`) and [`desktop-matrix.yml`](../../.github/workflows/desktop-matrix.yml) (unsigned builds and
Rust tests on Windows, Linux and macOS for every relevant PR, nightly). Design: 08-release §1–§3.

Humans hold every key and approve every release; the workflow cannot publish anything on its own.

## One-time setup (humans)

1. **Environment `release`** (Settings → Environments): required reviewer: the owner; deployment branches and tags limited to `main`, `desktop-v*`, `api-v*` and `runtimes-*` (checklist A1).
   Every job that reads a secret runs in it.
2. **Tag ruleset**: `desktop-v*`, `api-v*` and `runtimes-*` can be created only by maintainers, never
   deleted or moved.
3. **Updater key** (minisign), on an offline machine:
   ```sh
   pnpm --filter @vgames/desktop tauri signer generate -w vgames-updater.key
   ```
   Put the public key (the `.pub` file's content, a base64 string) in
   `apps/desktop/src-tauri/tauri.conf.json` → `plugins.updater.pubkey` through a normal PR. Store the private
   key file content as the secret `TAURI_SIGNING_PRIVATE_KEY` and its password as
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, both in the `release` environment only. Keep two offline copies:
   losing it means every launcher must be reinstalled by hand.
4. **Optional Windows code signing** (absent in v1): `WINDOWS_CERTIFICATE` (base64 of the `.pfx`) and
   `WINDOWS_CERTIFICATE_PASSWORD`. Authenticode signs the launcher, its installer and every overlay binary
   (`vgames_overlay64.dll`, `vgames_overlay32.dll`, the 32-bit inject helper).
5. **Optional Apple signing and notarization** (absent in v1): `APPLE_CERTIFICATE` (base64 `.p12` with the Developer ID Application certificate),
   `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific
   password), `APPLE_TEAM_ID`. The bundler signs and notarizes; the workflow checks `codesign`, the stapled
   ticket and Gatekeeper when notarization credentials are present. Without the OS signing secrets, the workflow emits a notice, builds unsigned and marks the draft "Unsigned build". This never disables updater signing.
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
     Windows), `tauri build` with OS signing when secrets exist and otherwise an unsigned-build notice; updater artifacts always re-signed for the version; OS signatures checked whenever signing ran.
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

**Build, sign and check by hand** (what the release workflow does):

```sh
cargo xtask runtimes build --previous last/runtimes.json --out runtimes.json
cargo xtask runtimes sign runtimes.json                        # reads the two variables above; writes runtimes.json.minisig
cargo xtask runtimes verify --last-version <published version> runtimes.json
```

## Scheduled workflows

| Workflow | When | What |
|---|---|---|
| [`e2e.yml`](../../.github/workflows/e2e.yml) | nightly, manual | The `vgames` CLI key pipeline against a real API (`scripts/e2e/key-pipeline.sh`), and the admin Playwright suite against the API serving the built admin web |
| [`desktop-matrix.yml`](../../.github/workflows/desktop-matrix.yml) | nightly, PRs touching the launcher crates | Unsigned builds and Rust tests on Windows, Linux and macOS |
| [`soak.yml`](../../.github/workflows/soak.yml) | weekly, manual | 24 h launcher leak test on a dedicated self-hosted runner (labels `self-hosted, linux, vgames-soak`); skipped until the repository variable `VGAMES_SOAK_RUNNER=true` |
| [Dependabot](../../.github/dependabot.yml) | weekly | Grouped updates; cryptography crates in their own PR; nothing is auto-merged |

## Automated release dry run

`release-dry-run.yml` runs weekly and on demand on hosted runners, without the protected release
environment, repository secrets or a GitHub release. Its first step checks the committed updater public
key and requires both `requireSignedVersion` and `createUpdaterArtifacts`; a placeholder key fails.
The same build steps used by the real release build unsigned Windows, Linux and macOS bundles.
Throwaway updater private keys stay within their build jobs and are deleted before artifacts upload.
Each artifact is signed for the workspace version; a second job downloads the bundles, public keys and
`latest.json`, then verifies all signatures and trusted versions independently.

On Ubuntu 24.04 the dry run installs the deb and runs `--smoke-test` once PLAY-09 provides it;
until then it checks the installed files. A second AppImage carries version `0.0.1` and the same
throwaway public key. The update round trip serves the compiled-in GitHub updater/changelog URLs over
loopback HTTPS with a throwaway trusted CA (no launcher override flag), then uses INS-09's real-app
harness to install and restart. It requires the installed AppImage to match the new artifact byte for byte,
the relaunched application's displayed version to match, and What's new to contain exactly the generated
player-facing entries. The smoke-test version assertion is added when PLAY-09 provides that command.
The hosts entry and throwaway keys are removed even on failure. Workflow artifacts contain bundles,
public keys, signatures and player changelog data only.

The server release remains keyless (cosign through GitHub OIDC) and needs no unsigned-build mode.
