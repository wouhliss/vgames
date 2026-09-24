# vgames

A fast, secure, server-based desktop launcher and package manager.

- **Launcher** (Tauri 2 + React): connect to any vgames server, sign in with Discord, install,
  update and play packages; cloud saves; friends, end-to-end encrypted chat and game invites;
  controller support across vendors.
- **Server** (Rust/axum + PostgreSQL 18 + Google Cloud Storage): catalog, signed-URL
  transfers, automatic metadata from IGDB and Steam, cloud saves, social relay, Swagger docs.
- **Admin web UI**: plain, robust management of packages, versions, users and trust.

Every package is signed by its publisher, and the launcher verifies every byte before writing or running it.
Downloads go straight to their final files at full bandwidth, with no archive to extract.

## Start here

| If you are… | Read |
|---|---|
| Anyone | [docs/architecture/00-overview.md](docs/architecture/00-overview.md) |
| An implementation agent | [AGENTS.md](AGENTS.md), then your task list in [docs/agents/](docs/agents/README.md) |
| Running a server | [infra/README.md](infra/README.md) |

## Local development

```sh
docker compose up -d          # Postgres 18 + GCS emulator
cp .env.example .env
pnpm install
cargo run -p vgames-api       # API on :8080, Swagger UI at /docs
pnpm dev:admin                # admin UI on :5173 (proxied to the API)
pnpm dev:desktop              # launcher (needs WebKitGTK 4.1 on Linux / WebView2 on Windows)
```

## Repository layout

```
apps/api            Rust API server (+ migrations)       crates/vgames-core      formats + crypto (shared)
apps/admin-web      admin SPA                            crates/vgames-proto     API/realtime DTOs
apps/desktop        launcher UI + src-tauri (Rust core)  crates/vgames-pack      packer (native + WASM)
openapi/            API contract (OpenAPI 3.1)           crates/vgames-transfer  download/upload engines
packages/api-client generated TS client                  crates/vgames-cli       `vgames` CLI
docs/               architecture + agent task lists      xtask/                  repo automation
```
