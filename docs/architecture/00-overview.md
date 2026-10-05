# vgames — System Architecture

Status: **v1 baseline** (2026-09-24). This document set is the contract that the
implementation agents build against. Change it through a PR titled
`contract: …` before changing code that contradicts it.

| Doc | Covers |
|---|---|
| [00-overview.md](00-overview.md) | Context, stack, components, repository layout, ownership, key flows, budgets |
| [01-security.md](01-security.md) | Threat model, algorithms, trust chain, auth, E2EE, hardening |
| [02-package-format.md](02-package-format.md) | Manifest, chunks/packs, signing, download/upload/update algorithms |
| [03-api.md](03-api.md) | API conventions, endpoint map, realtime protocol |
| [04-database.md](04-database.md) | Schema overview and invariants (DDL: `apps/api/migrations/`) |
| [05-social.md](05-social.md) | Friends, presence, E2EE messaging, invites, overlay |
| [06-cloud-saves.md](06-cloud-saves.md) | Save discovery, snapshots, sync and conflicts |
| [07-controllers.md](07-controllers.md) | Controller detection and cross-vendor emulation |
| [08-release.md](08-release.md) | CI/CD, launcher auto-update, user-facing changelog |
| [09-compatibility.md](09-compatibility.md) | Windows packages on Linux (Proton via umu) and macOS (Wine + Metal backends), runtime catalog, compat profiles |

---

## 1. What vgames is

vgames is a **server-dependent desktop launcher and package manager**. Anyone
can run a vgames server. A user points the launcher at a server URL, signs in
with Discord, and can then browse, install, update, launch and delete packages
published by that server's admins. The launcher also syncs save files through
the server, handles friends and end-to-end-encrypted chat, and sends game invites.

"Decentralized" means **federated-by-choice, not peer-to-peer**. Each server
is an independent island (its own Postgres, its own buckets, its own Discord
app, its own signing root). The launcher can remember several servers and
switch between them; there is no central vgames service, except the GitHub
releases feed that ships launcher updates.

```
                         ┌───────────────────── one vgames server ──────────────────────┐
┌──────────────┐  HTTPS  │ ┌──────────────┐   SQL   ┌────────────┐                       │
│   Launcher   │────────▶│ │  vgames-api  │────────▶│ PostgreSQL │                       │
│ (Tauri, Rust │◀──WSS───│ │ (axum, Rust) │         └────────────┘                       │
│  core + UI)  │         │ │  + job runner│  signs URLs   ┌──────────────────────────┐   │
└──────┬───────┘         │ │  + /admin UI │──────────────▶│ Google Cloud Storage     │   │
       │ signed URLs:    │ └──────▲───────┘               │ packages / saves / assets│   │
       │ direct range    │        │ HTTPS (cookie)        └────────────▲─────────────┘   │
       │ GETs / PUTs     │ ┌──────┴───────┐                            │                 │
       └─────────────────┼─┼──────────────┼────────────────────────────┘                 │
                         │ │ Admin browser│  (direct resumable uploads to GCS)           │
                         │ └──────────────┘                                              │
                         └───────────────────────────────────────────────────────────────┘
   External: Discord OAuth2 · IGDB (Twitch creds) · Steam Store API · GitHub Releases (launcher updates)
```

Bulk bytes (packages, saves) **never flow through the API**. The API issues
short-lived signed URLs, and clients talk to GCS directly. That is how
transfers can use the user's full bandwidth, and why the API stays small.

## 2. Technology choices

| Concern | Choice | Why |
|---|---|---|
| Launcher shell | **Tauri 2** (Rust + OS WebView) | Small binary, low RAM, native APIs from Rust. |
| Launcher UI | **React 19 + TypeScript 7 + Vite 8** | Mature, agents know it well. The WebView's own cost dwarfs the framework's. |
| Launcher ↔ UI contract | **tauri-specta** generated bindings | Typed commands and events; no stringly-typed `invoke`. |
| Backend | **Rust, axum 0.8, tokio** | The same language as the launcher lets the API, launcher, CLI and browser (WASM) share **one** implementation of manifests, signatures and path rules (`vgames-core`). A second implementation of a security format is a second attack surface. |
| Database | **PostgreSQL 18** via **sqlx 0.9** (compile-time checked queries, migrations) | Built-in `uuidv7()`, `SKIP LOCKED` job queue, `LISTEN/NOTIFY` fan-out. Postgres is the only stateful dependency. |
| API docs | **OpenAPI 3.1**, contract in `openapi/openapi.yaml`, served by **utoipa** + Swagger UI at `/docs` | Code-first annotations, with a CI check against the committed contract. |
| Object storage | **Google Cloud Storage** (official `google-cloud-storage` crate, V4 signed URLs, resumable uploads) + an `fs` backend for dev/tests and for self-hosters without GCS | Behind an `ObjectStore` trait. |
| Hashing | **BLAKE3** (content), SHA-256 (PKCE, token digests: interop) | BLAKE3 hashes at several GB/s per core, so verification never limits bandwidth. |
| Signatures | **Ed25519** (`ed25519-dalek` 3) | Small keys, fast verification, deterministic. |
| E2EE | **Olm** (Double Ratchet + 3DH) via **vodozemac** | Audited, Apache-2.0, pure Rust. libsignal is AGPL, which is incompatible with a closed-source client. |
| Local launcher DB | **SQLite** (`rusqlite`, bundled) | Libraries, installs, favorites, chat history, job journal. |
| Controllers | **SDL3** (input), **ViGEmBus** (Windows virtual pad), **uinput** (Linux), **CoreHID** (macOS 15+, entitlement-gated) | SDL3 has the widest vendor coverage (HIDAPI drivers). |
| In-game overlay | **hudhook** (Rust, D3D9/11/12 + OpenGL hooks, DLL injection) on Windows; own **Vulkan implicit layer** (Windows + Linux, covers Proton) + OpenGL preload on Linux; **NSPanel** over fullscreen Spaces on macOS; Dear ImGui UI | Packages have no anti-cheat, so in-process rendering is allowed and works in exclusive fullscreen. |
| Compatibility | **umu-launcher + UMU-Proton / GE-Proton** (Linux); **WineHQ macOS builds + D3DMetal / DXMT / DXVK-macOS** (macOS; D3DMetal redistributed because vgames is non-commercial); versions pinned by a signed runtime catalog | One Windows upload plays on all three OSes; native builds still win. |
| Admin UI | **React + Vite**, served by the API at `/admin/` | Same origin as the API: cookie auth, no CORS. |
| Lint/format | rustfmt + clippy; **Biome** for TS | Biome does not depend on the TypeScript compiler API, so it works with TS 7. |
| CI/CD | GitHub Actions | Humans provide secrets and runners. |

## 3. Components

### 3.1 Desktop launcher (`apps/desktop`)

Two layers with one hard rule:

> **The WebView is a view.** It never makes network requests, never touches
> the filesystem, never sees an access token or a private key. Every capability
> is a typed Tauri command implemented in Rust, and Rust validates every argument.

That rule keeps an XSS in the UI (for example a malicious package description)
from turning into token theft or arbitrary file writes. The CSP in
`tauri.conf.json` enforces it: `connect-src` allows only IPC.

Rust core modules (`src-tauri/src/`):

| Module | Responsibility |
|---|---|
| `servers` *(INS)* | Server profiles, `/.well-known/vgames.json` discovery, root-key pinning (TOFU), trust bundle cache. |
| `auth` *(INS)* | Discord login through the server (PKCE + deep link), tokens in the OS keychain, refresh rotation. |
| `api` *(INS)* | The only HTTP client. Retries, backoff, auth refresh, RFC 9457 error mapping. |
| `library` *(INS)* | Storage directories (multiple, user-chosen), install registry, favorites, categories (user collections). |
| `installs` *(INS)* | Install/update/repair/move/uninstall orchestration using `vgames-transfer`. |
| `launch` *(PLAY)* | Pre-launch verification, process spawn and tracking (Job Objects / process groups), playtime. |
| `shortcuts` + `deeplink` *(PLAY)* | `vgames://` registration and routing, desktop shortcuts. |
| `saves` *(PLAY)* | Cloud save snapshot/restore around launches. |
| `controllers` *(PLAY)* | SDL3 input thread, virtual pad backends, mapping profiles. |
| `compat` *(GAME)* | Runtime catalog, Proton (umu) and Wine runtimes, per-package prefixes, signed compat profiles, launch-plan wrappers. |
| `social` *(GAME)* | Realtime socket, friends, presence, Olm crypto, invites. |
| `overlay` *(GAME)* | In-game overlay broker (loopback IPC to the injected renderer), injection/layer setup at launch, macOS NSPanel, fallbacks. |
| `updater` *(INT)* | Launcher self-update via `tauri-plugin-updater` and the user-facing changelog. |
| `images` *(INS)* | `vgimg://` protocol serving cached cover art to the UI. |

Custom URI scheme `vgames://` (all inputs are untrusted; see 01-security §7):

| URI | Action |
|---|---|
| `vgames://launch/{package_id}` | Launch an installed, verified package. Desktop shortcuts use this. |
| `vgames://package/{server_id}/{package_id}` | Open package details (offer install). |
| `vgames://invite/{invite_id}` | Open an invite. |
| `vgames://server/add?url={https-url}&fp={root-fingerprint}` | Add a server with a pre-pinned root fingerprint. |
| `vgames://auth/callback?code=…&state=…` | Completes Discord sign-in. |

### 3.2 API server (`apps/api`)

A single stateless binary (scale horizontally behind a load balancer):

- REST `/v1/*` + `/.well-known/vgames.json`, OpenAPI at `/openapi.json`, Swagger UI at `/docs`.
- Realtime WebSocket `/v1/realtime`, fanned out across instances with Postgres `LISTEN/NOTIFY`.
- Job runner (same binary, `--role=worker` or both): metadata fetches, pack verification, save GC, expiry sweeps.
- Serves the built admin SPA at `/admin/`.

### 3.3 Admin web (`apps/admin-web`)

Plain, animation-free React SPA: packages, versions and uploads, metadata
review, users and roles, allowlist, trust and publisher keys, jobs, and the audit
log. Large uploads happen **in the browser**: a Web Worker runs `vgames-pack`
compiled to WASM (hashing, manifest, signing with the admin's publisher key)
and streams packs straight into GCS resumable sessions.

### 3.4 Shared crates

| Crate | Purpose | Owner |
|---|---|---|
| `vgames-core` | Manifest/trust formats, the chunk **layout function** (§4 of 02), Ed25519 domain-separated signatures, path safety, key files. No I/O, WASM-safe. | Agent 5 |
| `vgames-proto` | API DTOs and realtime event types (`openapi` feature derives utoipa schemas). | Agent 1 (+ Agent 4 for `social`) |
| `vgames-pack` | Deterministic packer (plan, hash, manifest). Native and WASM. | Agent 2 |
| `vgames-transfer` | Download and upload engines, update diffing. | Agent 2 |
| `vgames-cli` | `vgames` binary: key ceremonies, trust bundles, pack and publish. | Agent 5 (wiring) |
| `vgames-overlay` | In-game overlay renderer: Windows DLL (hudhook), Vulkan layer, Linux GL preload, Dear ImGui UI, IPC protocol shared with the launcher broker. | Agent 4 |
| `xtask` | Changelog tooling, OpenAPI drift check. | Agent 5 |

## 4. Repository layout

```
vgames/
├── AGENTS.md / CLAUDE.md        Rules every agent follows (read first)
├── Cargo.toml                   Rust workspace (+ shared dependency versions)
├── package.json                 pnpm workspace root (Biome, Redocly, TS)
├── docker-compose.yml           Postgres 18 + GCS emulator for local dev
├── .env.example                 Every API configuration variable, documented
├── .changes/                    Changelog fragments (one per PR) → 08-release.md
├── openapi/openapi.yaml         API contract (OpenAPI 3.1)
├── apps/
│   ├── api/                     vgames-api (axum)
│   │   ├── migrations/          sqlx migrations (append-only)
│   │   └── src/
│   ├── admin-web/               Admin SPA (served at /admin/)
│   └── desktop/                 Launcher UI (React)
│       └── src-tauri/           Launcher core (Rust), tauri.conf.json, capabilities/
├── crates/
│   ├── vgames-core/             Formats + crypto (shared, security-critical)
│   ├── vgames-proto/            API/realtime DTOs
│   ├── vgames-pack/             Packer (native + WASM)
│   ├── vgames-transfer/         Download/upload engines
│   ├── vgames-cli/              `vgames` CLI
│   └── vgames-overlay/          In-game overlay renderer (DLL / Vulkan layer / GL preload)
├── packages/
│   └── api-client/              TS client generated from openapi.yaml (admin-web)
├── xtask/                       `cargo xtask …` repo automation
├── runtimes/catalog.toml        Pinned compatibility runtimes (source of the signed runtimes.json)
├── scripts/release-notes/       Release-notes agent (Claude, structured output) for the user changelog
├── infra/                       What humans provision (buckets, CORS, IAM notes)
└── docs/
    ├── architecture/            This document set
    └── agents/                  Task lists for the implementation agents
```

## 5. Ownership map

Agents work in parallel. Every path has one owner. Phase 1 (2026-09-24 → 09-30) used five agents split by
technology; phase 2 (from 2026-10-05) uses **four vertical slices**, each owning the Rust, the UI and the tests
of its features, so that no feature waits on another agent. Roles, shared files and the "need it, build it"
rule: [docs/agents/phase-2/README.md](../agents/phase-2/README.md).

| Path | Owner (phase 2) |
|---|---|
| `apps/desktop/src-tauri/src/{api,servers,libraries,images,db}` and the new `{installs,catalog,downloads,publishing}`; `crates/vgames-pack/**`, `crates/vgames-transfer/**`, `packages/pack-wasm/**`; the onboarding, library, browse, package, downloads and publish screens and the Servers, Account, Storage and Downloads settings; `apps/desktop/e2e-real/**` | **INS** (install & library) |
| `apps/desktop/src-tauri/src/{launch,controllers,shortcuts,deeplink,logging,paths,events,error}` and the new `saves`; `src-tauri/{build.rs,tauri.conf.json,resources,icons}`; packaging; `apps/desktop/perf/**`; the General, About, Controllers and Cloud saves settings and the cloud-save dialogs | **PLAY** (launch & platform) |
| `apps/desktop/src-tauri/src/{social,overlay}` and the new `compat`; `apps/api/src/social/**`, social migrations, `crates/vgames-proto/src/{social,realtime}.rs`, the `social`, `messaging`, `invites` OpenAPI tags; `crates/vgames-overlay/**`, the new `crates/vgames-testapps/**`; `apps/desktop/src/overlay/**`, the friends screens and the Privacy, Overlay and Compatibility settings | **GAME** (in-game & social) |
| `apps/api/**` except `src/social/**`, `crates/vgames-proto/**` except social, non-social migrations and OpenAPI tags; `apps/admin-web/**`, `packages/api-client/**`; `crates/vgames-core/**`, `crates/vgames-cli/**`, `xtask/**`, `.github/**`, `.changes/` tooling, `runtimes/**`, `scripts/**`; `apps/desktop/src-tauri/src/updater/**` and the update screens; security docs; merges `contract:` PRs | **INT** (integrator) |
| Launcher registration points (`commands/mod.rs`, `commands/names.rs`, `capabilities/main.json`, `lib.rs`, `state.rs`, `db/migrations.rs`, generated `bindings.ts`) and the launcher UI foundation (`apps/desktop/src/{app,components,nav,i18n,ipc,mocks,styles}`, `apps/desktop/e2e/**`) | Shared (phase-2 README §5) |
| `docs/architecture/**`, `openapi/openapi.yaml`, `apps/api/migrations/**` | Contracts: any agent proposes a `contract:` PR, INT merges (append-only migrations) |

## 6. Key flows (summaries; details live in the linked docs)

**Add server.** User enters URL → launcher GETs `/.well-known/vgames.json` over
HTTPS → shows server name and **root key fingerprint** → user confirms (or it
matches the `fp` in a `vgames://server/add` link) → fingerprint pinned →
launcher fetches and verifies the trust bundle. A later fingerprint mismatch
blocks the server with a clear warning. There is no "continue anyway".

**Sign in.** Launcher creates a PKCE verifier → `POST /v1/auth/discord/start` →
opens the system browser → Discord → API callback → API redirects to
`vgames://auth/callback?code=…` → launcher exchanges code + verifier →
access and refresh tokens are stored in the OS keychain. (01-security §4)

**Publish.** Admin selects a folder (desktop admin mode, CLI or browser) →
packer plans chunks and packs → `POST /v1/admin/packages/{id}/versions` gets
`version_id` + `sequence` → packs stream to GCS resumable sessions in parallel
while being hashed → manifest built → **signed locally with the admin's
publisher key** → `finalize` → server checks the signature against the trust
bundle and queues a verification job that re-hashes every pack → `ready` →
admin publishes. (02-package-format §6)

**Install.** Launcher gets the release descriptor → downloads the manifest and
**verifies its signature before anything else** → checks free space and path
safety → preallocates files → parallel range GETs → each chunk BLAKE3-verified
in memory → written in place → journal → `install.json` committed last.
(02-package-format §7)

**Launch.** Install complete? → manifest still valid under the current trust
bundle (key not revoked)? → launch target's BLAKE3 matches? → pull cloud save
→ build the launch plan: native, or Proton/Wine with a verified runtime and the signed compat
profile (09-compatibility) → controller mapping → spawn (suspended on Windows while the overlay
DLL is injected; Vulkan layer / GL preload env on Linux) → track → on exit push cloud save.

**Invite.** Friend A invites B to package P → B's launcher shows an overlay
card → B accepts → if P is missing or outdated, B is prompted to install at once →
progress is visible to A → ready → the join secret goes to B E2E-encrypted.
(05-social §5)

## 7. Non-functional budgets (release blockers)

| Metric | Budget |
|---|---|
| Launcher cold start to interactive library | < 1.5 s (SSD, 1k installed packages) |
| Launcher idle RAM (main window, no game running) | < 200 MB total incl. WebView processes |
| Launcher idle CPU | ~0%: no polling timers; event-driven only |
| Launcher RSS growth over 24 h idle + 10 × 20 GB installs | < 5% (leak check, CI soak job) |
| Download throughput | ≥ 90% of link capacity up to 2.5 Gbit/s when disk allows; never CPU-bound on a 4-core machine |
| Download extra disk footprint | 0 bytes beyond final install size (+ journal < 1 MB per 100 GB) |
| Download memory | ≤ 256 MiB regardless of package size |
| Time to detect a corrupted chunk | before its bytes reach the target file |
| API p99 latency (non-bulk endpoints, warm) | < 100 ms at 200 rps per instance |
| Controller emulation added latency | < 2 ms p99 |
| In-game overlay cost per frame | ≤ 0.3 ms CPU + ≤ 0.3 ms GPU hidden; ≤ 1 ms with the panel open |
| Extra time to first frame with overlay / compat | overlay ≤ 150 ms; Proton or Wine launch overhead ≤ 3 s warm, excluding first prefix creation |

## 8. Cross-cutting conventions

- **Errors:** RFC 9457 `application/problem+json` everywhere, with a stable `code` field (03-api §2).
- **IDs:** UUIDv7; package slugs `^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$`.
- **Time:** RFC 3339 UTC strings on the wire; `timestamptz` in the database.
- **Logging:** `tracing` with JSON output in production. Never log tokens, codes, keys, message ciphertext, or signed URLs (query strings are redacted).
- **Configuration:** env vars validated at startup (`.env.example` is the reference). Fail fast.
- **Platforms:** Windows 10 22H2+ (x64, arm64), Linux x86_64 (glibc 2.35+, WebKitGTK 4.1), macOS 12+ (arm64, x64).
  Windows packages also run on Linux (Proton) and macOS (Wine) through 09-compatibility. macOS controller
  emulation needs macOS 15 and Apple's virtual-HID entitlement (07-controllers §4).
- **Accessibility:** every launcher screen is usable with keyboard only and with a gamepad only.
