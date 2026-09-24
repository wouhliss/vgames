# Agent 1 status

Backend & DB Architect (`apps/api`, `crates/vgames-proto` minus social/realtime, non-social migrations).

## Done
- A1-T01 — Service skeleton: config (every `.env.example` variable, all problems reported, exit 1),
  redacted logging (pretty/JSON), layer stack, problem+json errors, `/v1/health`, `/openapi.json`, `/docs`,
  graceful shutdown, `--role/--migrate/--check-config`, sqlx test harness. `/.well-known/vgames.json` is
  implemented but answers 503 until `vgames_core` provides the fingerprint (test `#[ignore]`d).
- A1-T02 — API conventions: `http::pagination` (HMAC-signed cursors, `PageParams`, `finish_page`),
  `http::idempotency` (`IdempotencyKey` extractor + `run`), `http::etag` (`etag`, `IfMatch::check` → 428/412),
  `http::ratelimit` (`RateLimits::check/check_n`, `Policy::*`; IP limits applied to every route),
  `http::query::Query<T>`, problem catalogue snapshot.
- A1-T03 — OpenAPI drift check: `apps/api/tests/openapi_contract.rs` compares the generated document
  (`vgames_api::http::openapi()`) with `openapi/openapi.yaml`; unimplemented contract operations are listed in
  `apps/api/tests/openapi_unimplemented.txt` (83 today, must only shrink). Reusable problem responses for
  handlers: `vgames_api::openapi_problems::{BadRequest, Unauthorized, NotFound, …}`.
- A1-T04 — Discord sign-in and sessions: start/callback/token/logout/me/sessions per 01-security §4
  (PKCE, login codes, refresh rotation with reuse detection, web cookies + CSRF + Origin, registration modes,
  bootstrap owner, disabled-user lockout). Dev fake Discord at `/v1/auth/dev/fake-discord`
  (`VGAMES_DEV_FAKE_DISCORD=true`, debug builds, localhost only). Migration `20260924120000_auth_session_fields`.
- A1-T05 — Realtime gateway: `POST /v1/realtime/ticket`, `GET /v1/realtime?ticket=` (single-use tickets in
  UNLOGGED `realtime_tickets`), 25 s ping / 60 s idle / 64 KiB frames, bounded per-socket queues (slow consumers
  closed with 4002), `LISTEN/NOTIFY` fan-out across instances (payloads > 7.5 KB by reference), `session.revoked`
  + close 4001 on logout / revoke / refresh reuse / disable, close 1012 on shutdown.
- A1-T06 — Job queue: `SKIP LOCKED` claims, 5 min leases with heartbeat, reaper, backoff `2^n × 10 s`,
  `dead` after `max_attempts`, dedupe keys, advisory-locked schedules (`sweep.expired` every minute,
  `saves.gc` daily). Workers run with `--role=all|worker`.
- A1-T07 — Object storage: `vgames_api::storage::Storage` (GCS via the official crate with V4 signed URLs;
  `fs` backend for dev/tests/self-hosting), conformance suite in `apps/api/tests/storage.rs`; `/v1/health` now
  reports storage.

## In progress
- A1-T08 — Trust endpoints (needs `vgames_core::trust` from Agent 5; continuing with A1-T09 meanwhile)

## Interfaces delivered (other agents may now rely on these)
- `vgames_api::error::ApiError` / `ApiResult` (problem+json), `vgames_api::http::json::{Json, Validate}`
  (rejects unknown fields, reports field paths) — for Agent 4's `social` handlers.
- `vgames_api::social::routes() -> OpenApiRouter<AppState>` mount point (convert `social.rs` to `social/mod.rs`).
- `state.limits.check(Policy::FriendRequests | Invites | Messages, key)` for Agent 4's per-action limits.
- **Agent 4:** when you implement a social/messaging/invites operation, remove it from
  `apps/api/tests/openapi_unimplemented.txt`; the drift test fails until the handler matches the contract.
- **Agent 5:** `cargo xtask openapi check` can run `cargo test -p vgames-api --test openapi_contract`.
- **Auth for every handler (Agents 4, 1):** `vgames_api::auth::{CurrentUser, RequireAdmin, RequireOwner, RequestMeta}`.
  `CurrentUser { user_id, role, session_id, device_id, kind }` accepts `Bearer vga_…` or the admin-web cookie
  (CSRF + Origin enforced on unsafe methods) and applies the per-user rate limit. `device_id` is `None` until the
  launcher registers a device (A4-T04 sets `sessions.device_id`).
- **Launcher sign-in (Agent 2, A2-T07):** `POST /v1/auth/discord/start` → browser → `vgames://auth/callback?code=…&client_state=…`
  → `POST /v1/auth/token`; see `apps/api/tests/auth.rs` for the exact flow. Refresh-token reuse returns
  `401 refresh_token_reused` and ends the session.
- **Realtime for Agent 4:**
  - Publish: `vgames_api::realtime::bus::publish(&pool, &Target::users(&[..]), "invite.created", data)`, or
    `publish_tx(&mut tx, …)` inside a transaction (delivered only on commit). `Target { users, sessions, close }`.
  - Inbound events: return `("presence.set", realtime::hub::handler(|ctx, data| async move { … }))` from
    `vgames_api::social::realtime_handlers()`; `ctx` has `state`, `user_id`, `session_id`, `device_id`.
  - Presence helpers: `state.realtime.is_connected(user_id)` (this instance only).
  - Envelope/ticket types: `vgames_proto::realtime::{Envelope, RealtimeTicket, Hello, SessionRevoked, close}`.
    **I created `crates/vgames-proto/src/realtime.rs`; it is yours to extend** with typed social events.
- **Jobs for Agent 4:** return `("social.sweep", jobs::handler(|ctx| async move { … }))` from
  `vgames_api::social::job_handlers()`; enqueue with `jobs::enqueue(&mut tx, kind, payload, Enqueue { dedupe_key, run_at, .. })`
  or `jobs::enqueue_now(&pool, …)`. Return `JobError::Retry` / `JobError::Fatal`. Social expiry (invites, friend codes,
  message envelopes) is yours to sweep; `sweep.expired` covers auth, realtime and idempotency tables.
- **fs storage protocol for Agent 2 (A2-T04/T05 tests):** run the API with `VGAMES_STORAGE_BACKEND=fs`; signed
  URLs point at `/_storage/…` on the API origin and behave like GCS: `GET` + `Range: bytes=a-b` → 206 with
  `Content-Range`; exact-length `PUT` with the signed `content-type`; resumable start = `POST` with the returned
  headers (`x-goog-resumable: start`, `x-goog-content-length-range`) → `201` + `Location`; chunks `PUT` with
  `Content-Range: bytes a-b/total|*` → `308` + `Range: bytes=0-n` until complete (`200`); progress query
  `Content-Range: bytes */*`. See `conformance()` in `apps/api/tests/storage.rs`.
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
