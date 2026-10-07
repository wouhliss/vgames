# What only the owner can do

Everything here is **non-blocking**: agents build and prove the automated path first, and each item says what it
unlocks. INT keeps this file current. An agent that finds a new step only a person can take adds it here, in
the same PR as the automated substitute.

## A. Paste and click (GitHub settings; about 20 minutes in total)

| # | When | Step | Unlocks |
|---|---|---|---|
| A0 | Completed for INT (2026-10-06); verify other sessions | Decide how agent sessions get their tool permissions. The README grants the *authority* (merge, push, install), but Claude Code still asks before tools unless the session runs in a permission mode that does not prompt, or the project allows them. To avoid prompts, either start each agent session in auto or bypass mode, or commit a project `.claude/settings.json` with an allow list (e.g. `Bash`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `WebFetch`, `mcp__github`) and deny rules for pushes to `main`. This is your call; the agents do not change their own permissions. | Agents that never stop on a permission prompt |
| A1 | Any time | **Environment `release`**: Settings → Environments → New `release`; required reviewer: you; deployment branches and tags: `main`, `desktop-v*`, `api-v*`, `runtimes-*`. **Tag ruleset**: Settings → Rules → New tag ruleset for `desktop-v*`, `api-v*`, `runtimes-*`: restrict creation to maintainers, block deletion and updates. | Real releases (until then, the dry-run workflow proves everything) |
| A2 | After INT sends you the key files (INT-08) | In `release` → Environment secrets, create `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `VGAMES_RUNTIME_CATALOG_KEY`, `VGAMES_RUNTIME_CATALOG_KEY_PASSWORD`, each with the content of the file of the same name. Keep two offline copies (password manager + offline backup), then delete the files from wherever you downloaded them. **Losing the updater key means every launcher must be reinstalled by hand.** | Signed runtime catalog releases, signed launcher updates |
| A3 | Ready: INT-03 merged in #145 | **Branch protection** for `main` (Settings → Branches): require a PR, require these exact checks (also listed in `docs/security/ci.md`): `Rust (fmt, clippy, tests)`, `SQLx offline data and migrations`, `Changelog fragments`, `WASM (vgames-core, pack-wasm)`, `TypeScript (Biome, typecheck, Vitest, OpenAPI lint)`, `Launcher UI end-to-end (mock mode)`, `Admin UI end-to-end (mock mode)`, `Desktop build check (Linux, WebKitGTK)`, `Desktop matrix gate`, `Supply chain (cargo-deny, cargo-audit, pnpm audit)`, `Secret scan (gitleaks)`, `Workflows (actionlint)`, `Dependency review`, require branches to be up to date, block force pushes. Agents already behave this way; this makes it enforced. | Enforcement only |
| A4 | Optional | `ANTHROPIC_API_KEY` in `release` (release-notes agent, 08-release §3.4). Without it, the linted player fragments are published verbatim. | Curated "What's new" text |
| A5 | Optional | Settings → Actions → General → "Allow GitHub Actions to create and approve pull requests". Without it, the runtime watcher opens issues instead of PRs. | Automatic runtime-update PRs |
| A6 | Last, after M4 is green | Push the tag `desktop-v0.1.0` on the commit INT names, approve the `release` deployment, review the draft release (notes, checksums), publish. Same for `api-v0.1.0`. | The first public release |

Not needed (owner decision 2026-10-05): Windows Authenticode certificate, Apple Developer ID and notarization,
Apple's virtual-HID entitlement, the D3DMetal intake. Builds ship unsigned; installers will show OS warnings
until a certificate is added (the workflow signs automatically once the secrets exist).

## B. Manual hardware checks (after M4; none of them gates a task)

Agents append each item with exact steps and the build to use. Expected list:

| # | Check | Why it is manual |
|---|---|---|
| B1 | Overlay over an **exclusive-fullscreen** D3D11 and D3D12 game on a Windows PC with a real GPU | Hosted runners have no GPU and no real fullscreen |
| B2 | macOS panel visible over a fullscreen-Space game without taking focus | Needs a visual check on a real Mac |
| B3 | A D3D11 game through Proton on Ubuntu 24.04 and on a SteamOS-like Arch install with a real GPU, overlay layer active | lavapipe proves correctness, not real drivers |
| B4 | A D3D11 game through Wine + DXMT and DXVK-macOS on an Apple silicon Mac (macOS 15 and 26), if the hosted runner had no Metal | Hosted macOS VMs may lack Metal |
| B5 | Cloud save round trip between a real Windows, Linux and Mac machine | Automated across runners already; this is a real-device confirmation |
| B6 | Fresh installs on clean Windows 11, Ubuntu 24.04 and macOS 14 machines (installer, deep links, shortcuts, uninstall) | Fresh-VM checklist (PLAY-09) |
| B7 | 24 h launcher idle soak and 8 h controller soak with two emulated pads | Hosted jobs stop at 6 h |
| B8 | Two physical controllers of different brands (e.g. DualSense, Switch Pro) mapped by position and by label in a real game | Needs real pads |

## C. For each vgames server you run (operations, not part of finishing the project)

Root key ceremony and first trust bundle (`docs/security/runbooks.md` §1), one publisher key per admin (§2), a
Discord application, IGDB/Twitch credentials if you want metadata, buckets per `infra/README.md`.
