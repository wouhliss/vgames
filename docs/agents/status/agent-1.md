# Agent 1 status

Backend & DB Architect (`apps/api`, `crates/vgames-proto` minus social/realtime, non-social migrations).

## Done
- A1-T01 — Service skeleton: config (every `.env.example` variable, all problems reported, exit 1),
  redacted logging (pretty/JSON), layer stack, problem+json errors, `/v1/health`, `/openapi.json`, `/docs`,
  graceful shutdown, `--role/--migrate/--check-config`, sqlx test harness. `/.well-known/vgames.json` is
  implemented but answers 503 until `vgames_core` provides the fingerprint (test `#[ignore]`d).

## In progress
- A1-T02 — API conventions

## Interfaces delivered (other agents may now rely on these)
- `vgames_api::error::ApiError` / `ApiResult` (problem+json), `vgames_api::http::json::{Json, Validate}`
  (rejects unknown fields, reports field paths) — for Agent 4's `social` handlers.
- `vgames_api::social::routes() -> OpenApiRouter<AppState>` mount point (convert `social.rs` to `social/mod.rs`).
- Test harness: `apps/api/tests/common/mod.rs` (`app(pool)`, `send`, `body_json`, `json_request`) with
  `#[sqlx::test(migrations = "./migrations")]`.
- Root `clippy.toml` allows unwrap/expect/panic/indexing in tests (AGENTS.md §5).

## Needs from others
- From Agent 5: `vgames_core` fingerprint (`VG1-…`) and key id functions (A5-T03) for `/.well-known/vgames.json`;
  `verify_bundle` / `verify_manifest` / `verify_compat_profile` (A5-T04) for trust and publishing endpoints.
- From Agent 2: `vgames_pack::verify::PackStreamVerifier` (A2-T03) for the `version.verify` job (A1-T12).

## Blockers / contract questions
- The GitHub token cannot create pull requests (`createPullRequest` not permitted), so I integrate with
  the fast-forward push path from the prompt.

## Local environment notes
- My tests use a dedicated Postgres 18 container on `127.0.0.1:55432` (`vgames-a1-pg`); ports 8080 and 4443
  are taken by other projects on this machine, so the dev API binds `127.0.0.1:8088` in my `.env`.
