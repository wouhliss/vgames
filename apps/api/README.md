# vgames-api

A single binary that runs:

- the REST API (`/v1/*`, contract in `openapi/openapi.yaml`, Swagger UI at `/docs/`);
- the realtime gateway (`/v1/realtime`);
- the job runner;
- the optional admin web UI (`/admin/`).

The design lives in `docs/architecture/` (00-overview, 01-security, 03-api, 04-database, 06-cloud-saves).
This file is the operator's view: configuration, roles, migrations, storage and the worker.

## Running it locally

```sh
docker compose up -d                                  # Postgres 18 + GCS emulator
cp .env.example .env                                  # fill the secrets (see below)
cargo run -p vgames-api -- --migrate                  # apply migrations, exit
cargo run -p vgames-api                               # http://localhost:8080
```

The binary reads `.env` from the working directory if present, then the process environment.

| Flag | Effect |
|---|---|
| `--check-config` | Validate the configuration, print `configuration OK` and exit (non-zero with every problem listed otherwise). |
| `--migrate` | Apply pending migrations with `DATABASE_MIGRATION_URL` and exit. |
| `--role all` (default) | API, realtime gateway and job runner in one process. |
| `--role api` | API and realtime gateway only. |
| `--role worker` | Job runner only. |

`GET /v1/health` answers `200` when the database and storage respond, `503` (`degraded` or `down`) otherwise. On `SIGTERM` or
Ctrl-C the process stops accepting connections and drains in-flight requests and jobs for up to 30 s. Realtime
sockets close with code 1012.

## Configuration

Everything is an environment variable. Invalid values stop the process at startup with a message naming
the variable. Secrets are never logged.

**Server**

| Variable | Default | Notes |
|---|---|---|
| `VGAMES_PUBLIC_URL` | required | The public origin (`https://games.example`). Used for redirects, the CSRF `Origin` check, CSP and signed `fs` URLs. |
| `VGAMES_SERVER_ID` | required | UUID identifying this server. Trust bundles are bound to it. |
| `VGAMES_SERVER_NAME` | `vgames` | Display name (≤ 100 characters). Owners can override it in the admin settings. |
| `VGAMES_BIND_ADDR` | `127.0.0.1:8080` | Listen address. Use `0.0.0.0:8080` in a container. |
| `VGAMES_TRUST_PROXY_HEADERS` | `false` | Take the client IP from `X-Forwarded-For`. Enable **only** behind a proxy that overwrites that header, or rate limits can be bypassed. |
| `VGAMES_LOG` | `info` | `tracing` filter, e.g. `info,vgames_api=debug`. |
| `VGAMES_LOG_FORMAT` | `pretty` | `pretty` or `json`. |
| `VGAMES_ADMIN_DIST` | unset | Directory of the built admin web UI (`apps/admin-web/dist`, must contain `index.html`). Unset: no `/admin/`. |

**Keys**

| Variable | Default | Notes |
|---|---|---|
| `VGAMES_ROOT_PUBLIC_KEY` | required | Base64 Ed25519 public key of the offline root (`vgames keys init-root`). Every trust bundle must be signed by it. |
| `VGAMES_SERVER_SECRET` | required | Base64, ≥ 32 random bytes (`openssl rand -base64 32`). Derives the key that signs page cursors; rotating it only makes outstanding cursors invalid (clients start again from the first page). |

**Database**

| Variable | Default | Notes |
|---|---|---|
| `DATABASE_URL` | required | `postgres://` URL of the runtime role. |
| `DATABASE_MIGRATION_URL` | `DATABASE_URL` | Role that owns the schema, used only by `--migrate`. |
| `VGAMES_DB_MAX_CONNECTIONS` | `20` | Pool size per process (1–500). |

**Discord sign-in**

| Variable | Default | Notes |
|---|---|---|
| `DISCORD_CLIENT_ID` | required | Discord application id. |
| `DISCORD_CLIENT_SECRET` | required | |
| `DISCORD_REDIRECT_URI` | required | Must be exactly `<VGAMES_PUBLIC_URL>/v1/auth/discord/callback`. Register the same URL in the Discord application. |
| `VGAMES_BOOTSTRAP_OWNER_DISCORD_ID` | unset | Discord user id admitted whatever the registration mode, and made owner while the server has none. |
| `VGAMES_DEV_FAKE_DISCORD` | `false` | Debug builds only, and only with a localhost `VGAMES_PUBLIC_URL`: a fake identity provider for development and CI. Release builds refuse it. |

**Storage** (see below)

| Variable | Default | Notes |
|---|---|---|
| `VGAMES_STORAGE_BACKEND` | required | `gcs` (production) or `fs` (development and tests). |
| `GCS_BUCKET_PACKAGES`, `GCS_BUCKET_SAVES`, `GCS_BUCKET_ASSETS` | required for `gcs` | Three private buckets. |
| `GOOGLE_APPLICATION_CREDENTIALS` | unset | Service-account key file. Unset: workload identity / metadata server. |
| `STORAGE_EMULATOR_HOST` | unset | GCS emulator URL (local `docker compose`). |
| `VGAMES_FS_STORAGE_ROOT` | required for `fs` | Directory holding the objects. |
| `VGAMES_FS_URL_SIGNING_KEY` | required for `fs` | Base64, ≥ 32 bytes. Signs the `/_storage/*` URLs. |
| `VGAMES_SIGNED_URL_TTL_SECONDS` | `21600` (6 h) | Lifetime of download URLs, 60 s – 7 days. |
| `VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE` | `536870912` (512 MiB) | Cloud-save quota per user and package (≥ 1 MiB). |

**Metadata and jobs**

| Variable | Default | Notes |
|---|---|---|
| `IGDB_CLIENT_ID`, `IGDB_CLIENT_SECRET` | unset | Twitch credentials for IGDB metadata. Both or neither. |
| `VGAMES_METADATA_STEAM_ENABLED` | `true` | Look up Steam store metadata. |
| `VGAMES_WORKER_CONCURRENCY` | `4` | Jobs run in parallel per worker process (1–64). |

## Roles

Accounts come only from Discord sign-in. Each has one role:

| Role | Can |
|---|---|
| `user` | Browse the catalog, download, cloud saves, social features. |
| `admin` | Everything a user can, plus: packages, versions and uploads, metadata, compatibility profiles, the allowlist, jobs, the audit log, reading server settings, and disabling or enabling **users** (not admins or owners). |
| `owner` | Everything an admin can, plus: change anyone's role, disable admins, change server settings (registration mode, MOTD, name) and upload trust bundles. |

- The first owner is `VGAMES_BOOTSTRAP_OWNER_DISCORD_ID`. After that, owners promote others from the admin UI
  (`PATCH /v1/admin/users/{id}`).
- The last active owner can be neither demoted nor disabled (`409 last_owner`), and nobody can disable themselves.
- Disabling a user revokes all their sessions at once and closes their realtime sockets.
- **Registration mode** (server setting, default `allowlist`) decides who can create an account on first
  sign-in: `open` (any Discord account), `allowlist` (Discord ids an admin added) or `closed` (nobody).
  Existing accounts are not affected.
- Every admin or owner change is written to the audit log in the same transaction.

## Migrations

- Migrations live in `apps/api/migrations/` and are embedded in the binary. Run `vgames-api --migrate` before
  starting a new version: the server does not migrate on startup. It uses `DATABASE_MIGRATION_URL`, so the
  runtime role in `DATABASE_URL` does not need to own the schema.
- A change is always a new file (`YYYYMMDDHHMMSS_what.sql`); never edit a merged one. Social tables belong to
  Agent 4, everything else to Agent 1 (AGENTS.md §2).
- Queries are compile-time checked (`sqlx::query!`). After changing a query or the schema, apply the
  migration to your dev database and run `cargo sqlx prepare --workspace`, then commit `.sqlx/`. CI builds
  with `SQLX_OFFLINE=true` and checks that `.sqlx/` is current.
- Every pool connection sets `plan_cache_mode = force_custom_plan` (`db::SESSION_OPTIONS`): list queries take
  optional filters as `($n IS NULL OR …)` and need per-call plans to use their indexes. `apps/api/bench/`
  has the seed data and `EXPLAIN` script to check a query against 100k packages and 1M audit rows.

## Storage backends

Bulk bytes (packs, manifests, saves, images) never pass through the API. It signs short-lived URLs, and
clients talk to storage directly.

- **`gcs`** (production). Three private buckets; the API signs V4 URLs with its service account. Uploads use
  resumable sessions, and downloads support `Range`. The service account needs object read/write/delete on
  the three buckets and permission to sign (e.g. `roles/iam.serviceAccountTokenCreator` on itself when
  using workload identity).
- **`fs`** (development, tests, single-machine trials). Objects live under `VGAMES_FS_STORAGE_ROOT` and are
  served by the API itself at `/_storage/*` with HMAC-signed URLs that behave like GCS (ranges,
  resumable uploads). They have their own rate limit (6,000 requests/min per IP). Not meant for production
  traffic.

## Worker

The job runner is a Postgres queue (`jobs` table):

- Claims use `FOR UPDATE SKIP LOCKED` with a 5-minute lease kept alive by a heartbeat. A reaper requeues
  jobs of crashed workers.
- Failures retry with exponential backoff and become `dead` after `max_attempts`. Admins see them and can
  retry (`/v1/admin/jobs`).
- `dedupe_key` prevents duplicate queued or running jobs.

Any number of worker processes can run against one database (`--role worker`, or the default `--role all`).
Periodic jobs are enqueued once per period cluster-wide.

| Kind | What it does |
|---|---|
| `metadata.fetch`, `metadata.images` | IGDB/Steam metadata candidates and image import for a package. |
| `version.verify` | Verifies an uploaded version's packs against its signed manifest. |
| `version.cleanup` | Deletes every stored object of an aborted version. |
| `pack.reverify` | Re-checks a pack reported corrupt by clients. |
| `sweep.expired` | Every minute: expired sessions, sign-in flows, login codes, realtime tickets and events, idempotency keys; succeeded jobs after 7 days. |
| `saves.gc` | Daily: trims save history to the retention and deletes unreferenced save blobs (1-day grace). |
| `social.*` | Agent 4's social jobs (invites, friend codes, message envelopes expiry). |

## Tests and tooling

- `cargo test -p vgames-api`: unit tests, and the integration suite in `tests/it/` (one binary, each test on
  its own database via `#[sqlx::test]`). Needs `DATABASE_URL` pointing at a Postgres 18 where the test role
  can create databases.
- `tests/openapi_contract.rs` fails when the generated OpenAPI document drifts from `openapi/openapi.yaml`.
  `tests/openapi_unimplemented.txt` is empty: every contract operation has a handler.
- `tests/it/limits.rs` walks every route and fails if one is not rate limited.
- `apps/api/bench/`: seeded query plans and an `oha` load test (`README.md` there).
