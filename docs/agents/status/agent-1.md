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
  `fs` backend for dev/tests/self-hosting), conformance suite in `apps/api/tests/it/storage.rs`; `/v1/health` now
  reports storage.
- A1-T09 — Packages, assets and catalog: admin CRUD (`/v1/admin/packages[/{id}]`, Idempotency-Key on create,
  `If-Match` on PATCH/DELETE, slug generation with `-2…` suffixes, soft delete keeps the slug), image upload
  (`POST /v1/admin/packages/{id}/assets`: JPEG/PNG/WebP ≤ 10 MiB, ≤ 16384 px per side, re-encoded, sha256 dedupe,
  first cover/hero/logo becomes the default), `DELETE /v1/admin/assets/{id}`, public catalog
  (`GET /v1/packages` with `q`/`genre`/`platform`/`sort` and signed cursors, `GET /v1/packages/{id}`),
  `GET /v1/assets/{id}` → 302 to a signed URL. Migration `20260924140000_catalog_search` (trigram title index).

- A1-T10 — Metadata fetching: `metadata.fetch` job (IGDB with a cached Twitch token at ≤ 4 req/s, Steam
  storesearch/appdetails at ≤ 1 req/s; lookup by explicit IGDB id, else by Steam id, else by title), normalized
  candidates (HTML → text, clamped, title score), auto-apply only for explicit ids or one unambiguous ≥ 0.95 match
  (never over `admin` fields), ProtonDB tier on the package and umu id on the Steam candidate (best effort).
  `POST …/metadata/refresh` (202, deduplicated), `GET …/metadata/candidates`, `POST …/metadata/apply` (If-Match,
  `overwrite_admin_fields`). `metadata.images` job downloads through `metadata::safe_fetch` (HTTPS, host allowlist,
  resolver refusing private/loopback/link-local, same-host redirects only, 10 MiB, 15 s) into the T09 image pipeline.
- A1-T08 — Trust endpoints: `GET /v1/trust/bundle` (public, `max-age=60`), `POST /v1/admin/trust/bundles`
  (owner; `vgames_core::trust::verify_bundle` under `VGAMES_ROOT_PUBLIC_KEY` moved along the stored `next_root`
  chain; versions only go up; holders must be active admins/owners; bytes stored verbatim; `publisher_keys`
  upserted in the same transaction, revocations applied; audited), `GET /v1/admin/trust/publisher-keys`.
  `/.well-known/vgames.json` now serves the `VG1-…` fingerprint (the ignored test runs again).

- A1-T11 — Version upload protocol: create/list/get/abort versions, pack and manifest upload targets (#13);
  `POST …/finalize` (manifest size + BLAKE3, `vgames_core::verify::verify_manifest` in server mode against the current
  trust state, every pack present with its manifest size, `manifest.sig` stored, `package_packs` inserted, state →
  `verifying`, `version.verify` queued) and `POST …/signature` (re-sign over the stored manifest). 422 codes:
  `manifest_hash_mismatch`, `manifest_missing`, `signature_invalid`, `publisher_key_untrusted`,
  `publisher_key_not_yours`, `publisher_key_expired`, `manifest_invalid`, `manifest_mismatch`, `pack_missing`,
  `pack_size_mismatch`.

- A1-T12 — Verification, publishing, downloads: `version.verify` job, publish, yank (#18); release descriptor,
  download URLs (`410 version_yanked`), integrity reports → `pack.reverify` (#19); compat profiles:
  `GET /v1/packages/{id}/compat` (latest revision per target) and `PUT /v1/admin/packages/{id}/compat/{target}`
  (`vgames_core::verify::verify_compat_profile`, revision must grow → `409 stale_revision`, `422 compat_mismatch`,
  `422 compat_invalid`, key errors as for finalize).

- A1-T13 — Cloud saves (06 §4): `GET …/head` (`404 no_saves`), `GET …/snapshots` (newest first, signed cursors),
  `GET …/snapshots/{id}`, `POST …/blobs/prepare` (quota, missing blobs only, exact-length signed PUTs, 15 min),
  `POST …/blobs/download-urls`, `POST …/snapshots` (Idempotency-Key; compare-and-swap on the parent →
  `409 save_head_conflict` naming the current head in `detail` and `errors[0].message`; `409 blob_not_uploaded`;
  `413 save_quota_exceeded`). `saves.gc`: newest 20 snapshots + head, unreferenced blobs after one day.
  Contract commit: 404 on prepare/commit for unknown packages, quota rule in 06 §4.

- A1-T14 — Admin server endpoints: `GET /v1/admin/users` (`q` = username/display name substring or exact Discord id,
  `role`, signed cursors), `PATCH /v1/admin/users/{id}` (role: owner only; `409 last_owner`; admins cannot disable or
  enable admins/owners; `403 cannot_disable_self`; disabling revokes every session and sends `session.revoked` + close
  4001 on commit), allowlist `GET/POST/DELETE` (`409 already_allowlisted`), settings `GET` (admins) / `PATCH` (owner,
  `If-Match` → 428/412; `name` overrides `VGAMES_SERVER_NAME`, also in `/.well-known/vgames.json`; empty `motd` clears),
  `GET /v1/admin/jobs` (`state`, `kind`) and `POST …/retry` (failed/dead only → `409 job_not_retryable`,
  `409 job_already_queued`), `GET /v1/admin/audit-log` (actor, action, target, since/until). All mutations audited.
  Authorization matrix test: every contract `/v1/admin/*` operation × anonymous/user/disabled/admin/owner; it found
  and fixed a plain-text rejection on non-multipart asset uploads (now `415` problem+json).

- A1-T15 — Admin web hosting: `VGAMES_ADMIN_DIST` served at `/admin/` (`/admin` → 308). Client routes fall back to
  `index.html` (`no-store`); `assets/*` is `public, max-age=31536000, immutable`; other files `no-cache`; every response
  carries the admin CSP (01-security §5); `.`/`..`/hidden/empty segments, backslashes and colons are refused and the
  resolved file (after symlinks) must stay inside the canonical root. A missing directory or one without `index.html`
  fails config validation at startup.

- A1-T16 — Hardening and performance (wouhliss/vgames#36, wouhliss/vgames#45, and the catalog follow-up PR).
  - Rate limits on every route, problem+json for every error, property tests (#36).
  - `apps/api/bench/`: seed (100k packages, 1M audit rows, 1M envelopes, 20k sessions, 200k jobs),
    `explain.sql` (the handlers' SQL; `-v generic=1` for generic plans) and `load.sh` (`oha`).
  - `db::connect` sets `plan_cache_mode = force_custom_plan`: list filters are `($n IS NULL OR …)`,
    which generic plans cannot index (catalog 39–173 ms, filtered audit 123–176 ms before).
  - Indexes: audit actor/action/target and job state on `(key, id DESC)`; GIN on `packages.genres`.
  - Catalog: `sort=recent` pages by `(updated_at, id) DESC`, matching its cursor (it could repeat or
    skip packages sharing an `updated_at`); genre filter `genres @> ARRAY[$genre]`; searches sort
    their matches before checking for a release.
  - **Plans** (`explain.sql`, custom plans, warm; no sequential scan on a hot path):

    | Query | Time |
    |---|---|
    | Session lookup | 0.1 ms |
    | Catalog first or cursor page (title or recent) | 0.2–0.5 ms |
    | Catalog, linux only / genre + linux | 4 / 5 ms |
    | Catalog, rare genre | 0.5 ms |
    | Catalog search: rare / two words / broad one word by recency / by title | 1 / 10 / 21–35 / ~40 ms |
    | Release descriptor, pack lookup | 0.2 ms |
    | Inbox (Agent 4's query) | 0.8 ms |
    | Audit, any filter | ≤ 0.4 ms |
    | Job claim / admin job list | 0.07 / 0.1 ms |

  - **Load** (`load.sh`, release build, 200 rps for 30 s after a 10 s warm-up, 25 sessions,
    latency-corrected; 4 cores shared by Postgres, the API and the load generator):

    | Endpoint | p50 | p99 | Budget (100 ms) |
    |---|---|---|---|
    | Release descriptor | 4–5 ms | 10–12 ms | met |
    | Download URLs | 4–5 ms | 9–10 ms | met |
    | Catalog | 18–21 ms | 107–194 ms over four warm runs | **not met** |

    The catalog mix is 25% searches, half of them one common word matching ~12% of 100k titles.
    Such a search costs ~40 ms of CPU: a trigram recheck of ~12k rows plus a sort under the
    `en_US` collation. Plans that walk the title index instead are faster for words spread
    through titles, but can scan most of the index for a title's first word (its matches sit
    in one alphabetical block): `'%shadow quest%'` took 235 ms that way. So I kept the bounded
    plan. Browsing, filters and specific searches are well inside the budget. A faster broad
    search needs a product decision (ranked or capped results, or byte-order title sorting).
  - Inbox budget: Agent 4's endpoint (A4-T05); its query runs on `message_envelopes_inbox_idx` in 0.8 ms.

## In progress
- A1-T17 — Handoff: `apps/api/README.md` written, interfaces listed below, `.sqlx/` current (CI checks it).
  The drift allowlist (`apps/api/tests/openapi_unimplemented.txt`) holds only Agent 4's 11 conversation, inbox
  and invite operations; it empties as Agent 4 implements them (A4-T05, A4-T06).

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
  → `POST /v1/auth/token`; see `apps/api/tests/it/auth.rs` for the exact flow. Refresh-token reuse returns
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
  `Content-Range: bytes */*`. See `conformance()` in `apps/api/tests/it/storage.rs`.
- **Catalog for Agent 3 (library/store views) and Agent 2 (launcher client):** `GET /v1/packages`,
  `GET /v1/packages/{id}` and the admin package endpoints match `openapi/openapi.yaml`; asset `url`s are
  `/v1/assets/{id}` and answer `302` to a short-lived signed URL (`Cache-Control: private, max-age=3600`), so
  fetch them with the session token. Catalog lists only `published` packages with at least one release.
- **Metadata for Agent 3 (admin UI):** candidates come from `GET /v1/admin/packages/{id}/metadata/candidates`
  (`items` best score first, plus the latest `job`); a Steam candidate may carry `data.external.umu_id`
  (contract PR #6), show it as a compat-profile hint. `AdminPackage.protondb_tier` is filled when the Steam app is
  known. Poll `metadata_job.state` on the package after create/refresh.
- `vgames_proto::packages` holds the package/asset/catalog DTOs (`PackageSummary`, `PackageDetail`,
  `AdminPackage`, `AdminPackagePatch` with `null`-clears semantics).
- **Trust for Agent 2 (launcher) and Agent 3 (admin UI):** `GET /v1/trust/bundle` returns
  `{bundle, signature}` (base64 of the exact bytes) with no auth; feed it to `vgames_core::trust::verify_bundle`.
  `/.well-known/vgames.json` `root_key_fingerprint` is `PublicKey::fingerprint()` of the configured root.
  Admin upload errors: 422 `bad_signature` / `wrong_server` / `invalid_bundle` / `unknown_holder`
  (`errors[].field = publishers[i].holder_user_id`), 409 `stale_version`.
- `Config::root_public_key` is now a `vgames_core::PublicKey` (off-curve and small-order keys fail at startup).
- **Uploads for Agent 2 (packer/launcher admin mode) and Agent 3 (browser worker):** create a version, then for each
  pack `POST …/packs/{i}/upload-session` → `UploadTarget {url, method: POST, headers, expires_at}`: send the POST with
  exactly those headers, take `Location` as the session URI, `PUT` chunks with `Content-Range`. The manifest target is a
  single `PUT` with `content-type: application/json` and `x-goog-content-length-range: 1,268435456`. Objects:
  `v1/{package}/{version}/packs/{i:05}.pack`, `…/manifest.json` (`vgames_api::versions::{pack_object, manifest_object}`).
- **Downloads for Agent 2 (launcher):** `GET /v1/packages/{id}/releases/{platform}` → `ReleaseDescriptor`
  (`manifest.{url,size,blake3,expires_at}`, `signature` envelope for `vgames_core::verify::verify_manifest`,
  `yanked_version_ids`); then `POST /v1/versions/{id}/download-urls {packs:[…]}` → `{items:[{pack_index,url,size,
  expires_at}]}` (URLs live `VGAMES_SIGNED_URL_TTL_SECONDS`, default 6 h). Report a chunk seen corrupt twice with
  `POST /v1/versions/{id}/integrity-reports {pack_index, chunk_index?, detail?}`.
- **Cloud saves for Agent 2 (launcher `saves` module, A2 save sync) and Agent 3 (Settings → Cloud saves history):**
  push = `POST /v1/saves/{package}/blobs/prepare {blobs:[{blake3,size}]}` → `{missing:[{blake3,url,method:"PUT",headers,
  expires_at}]}` (send each PUT with exactly those headers; blobs the server already has are not listed) →
  `POST /v1/saves/{package}/snapshots {parent_snapshot_id (null only for the first save), platform, label?, files:[{root,
  path,size,blake3,mtime}]}` → `201` new head, or `409 save_head_conflict` (another device pushed; `errors[0].message`
  is the current head id, or `none`) → conflict dialog; `409 blob_not_uploaded` → prepare and upload again;
  `413 save_quota_exceeded`. Pull = `GET …/head` (`404 no_saves` when there is nothing yet) →
  `POST …/blobs/download-urls {blake3:[…≤1000]}`. Send an `Idempotency-Key` on commits so a retry after a lost response
  returns the same snapshot instead of a conflict. `device_id` comes from the session (A4-T04), not the body.
  Paths are checked with `vgames_core::paths` rules per save location (no case collisions); up to 10,000 files per snapshot.
- **Rate limits and errors for everyone (A1-T16):** every route is limited. Routes that authenticate (bearer/cookie in
  the OpenAPI document) are limited per user when the request carries a valid session; everything else (public routes,
  unknown paths, requests without or with bad credentials) per IP, `/_storage/*` under its own `Policy::Storage`
  (6,000/min per IP) and `/admin/*` + `/docs/*` under `Policy::Static` (3,000/min per IP). `tests/it/limits.rs` walks every operation, so a new route without a limit fails CI.
  Any non-JSON error on `/v1/*` is rewritten to problem+json by `http::problems::normalize` (same status, original
  text in `detail`). **Agent 4:** prefer `vgames_api::http::path::Path` over `axum::extract::Path` in `social/`: a
  malformed id then answers `400 invalid_path` with a readable detail instead of the generic rewrite.
- **Admin web hosting for Agent 3 (admin-web build):** build with Vite `base: "/admin/"` and emit hashed files into
  `assets/` (Vite's default); everything else in `dist/` is served `no-cache`. Point `VGAMES_ADMIN_DIST` at `dist/`.
  The CSP allows `'wasm-unsafe-eval'` and `worker-src 'self'` for the upload worker, `connect-src`/`img-src` for
  `https://storage.googleapis.com`; no inline scripts or styles, no other origins.
- **Admin server for Agent 3 (admin web users/allowlist/settings/jobs/audit screens):** everything under the
  `admin-server` tag now matches `openapi/openapi.yaml`; DTOs in `vgames_proto::admin` (`AdminUser`, `AdminUserPatch`,
  `AllowlistEntry`, `ServerSettings`, `ServerSettingsPatch`, `AuditEntry`) and `vgames_proto::jobs::JobPage`. Problem codes
  to handle: `last_owner`, `cannot_disable_self`, `already_allowlisted`, `job_not_retryable`, `job_already_queued`,
  `precondition_required` / `precondition_failed` (settings). Admins can read settings; only owners get a `200` on PATCH.
- `vgames_proto::saves` holds the cloud-save DTOs (`SaveSnapshot` flattens `SaveSnapshotSummary`).
- **Catalog order for Agent 3 (store/library views) and Agent 2 (launcher client):** `GET /v1/packages` pages are
  total and stable: `sort=title` by lowercase title then id, `sort=recent` by `updated_at` then id, both
  descending for recent. Follow `next_cursor` until it is `null`; a cursor only works with the filters it came
  from (`400` otherwise). `genre` matches one entry of `genres` exactly.
- **Database access for Agent 4 (`social/`):** pools come from `vgames_api::db::connect` and plan every
  statement with its parameters (`db::SESSION_OPTIONS`), so optional filters written `($n IS NULL OR col = $n)`
  still use your indexes. Check a new hot query with `apps/api/bench/explain.sql` on the seeded database.
- `apps/api/README.md`: operator guide (every environment variable, roles, `--migrate`, `--role`, storage
  backends, job kinds and schedules).
- `Storage::sign_put_range(bucket, name, ttl, content_type, min, max)` (both backends).
- Test harness: `apps/api/tests/it/common/mod.rs` (`app(pool)`, `send`, `body_json`, `json_request`) with
  `#[sqlx::test(migrations = "./migrations")]`.
  All API integration tests are modules of one binary (`apps/api/tests/it/main.rs`); add new files there, not
  as new `tests/*.rs` targets (each separate target is another ~440 MB debug binary, which filled CI disks).
- Root `clippy.toml` allows unwrap/expect/panic/indexing in tests (AGENTS.md §5).

## Needs from others
- From Agent 4: the conversation, inbox and invite operations, to empty the drift allowlist (A1-T17).

## Blockers / contract questions
- None. I merge my own PRs (rebase merge) once Agent 5's CI is green. Seen Agent 5's two CI notes (2026-09-26):
  Actions ran out of minutes, then the repo went public and GitHub CI is back as the merge gate;
  `scripts/ci/local.sh` is optional.
- Contract commit (A1-T13, in the same PR as the feature): `prepareSaveBlobs` / `commitSaveSnapshot` answer 404 for an
  unknown package; 06 §4 now defines the quota as head + pushed + in-flight blobs, with commits trimming the oldest
  history (never the head) to fit, so a large save that changes every session never locks a player out.
- Follow-up contract fix: `DELETE /v1/admin/packages/{id}` returns 428 without `If-Match` but does not list it;
  I will add it (contract + handler) in a small `contract:` PR.

## Local environment notes
- My tests use a dedicated Postgres 18 container on `127.0.0.1:55432` (`vgames-a1-pg`); ports 8080 and 4443
  are taken by other projects on this machine, so the dev API binds `127.0.0.1:8088` in my `.env`.
