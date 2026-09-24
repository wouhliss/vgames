# Infrastructure — what humans provision

Agents implement integrations; people own accounts, secrets and infrastructure.
Everything the code expects is listed here. Configuration reference: [`.env.example`](../.env.example).

## 1. Per vgames server

### Google Cloud Storage
Create **three private buckets** in the same region as the API:

| Env var | Contents | Notes |
|---|---|---|
| `GCS_BUCKET_PACKAGES` | manifests, signatures, packs | Largest; Standard class |
| `GCS_BUCKET_SAVES` | cloud save blobs | Consider Object Versioning off (the app versions saves) |
| `GCS_BUCKET_ASSETS` | cover art, screenshots | Small |

For each bucket:
- Uniform bucket-level access **on**, public access prevention **enforced** (all access is via signed URLs).
- CORS: `gcloud storage buckets update gs://BUCKET --cors-file=infra/gcs/cors.json`
  (replace the origin with the server's public host; needed for admin-web uploads from the browser).
- Lifecycle: `gcloud storage buckets update gs://BUCKET --lifecycle-file=infra/gcs/lifecycle.json`.

Service account for the API:
- `roles/storage.objectAdmin` on the three buckets (not project-wide).
- Signed URLs: either a JSON key (`GOOGLE_APPLICATION_CREDENTIALS`) or, on Cloud Run/GKE with
  workload identity, `roles/iam.serviceAccountTokenCreator` on **itself** (signBlob).

### PostgreSQL 18+
- Managed instance (e.g. Cloud SQL for PostgreSQL 18). TLS required.
- Two roles: `vgames_owner` (runs migrations, owns tables) and `vgames_app` (connect + DML only),
  with `DATABASE_URL` using `vgames_app`, `DATABASE_MIGRATION_URL` using `vgames_owner`.
- Automated backups + point-in-time recovery.

### Discord application
- Create an application at the Discord developer portal → OAuth2.
- Redirect: `https://<public host>/v1/auth/discord/callback` (exact match).
- Scope used: `identify` only. Put the client id and secret in `DISCORD_CLIENT_ID` / `DISCORD_CLIENT_SECRET`.

### IGDB (optional, recommended)
- Register a Twitch application; use its client id and secret as `IGDB_CLIENT_ID` / `IGDB_CLIENT_SECRET`.

### Root key ceremony (server owner, offline)
1. On an offline machine: `vgames keys init-root --out root.vgkey` (asks for a passphrase).
2. Copy the printed public key into `VGAMES_ROOT_PUBLIC_KEY`, and publish the fingerprint
   (`VG1-…`) wherever your users will see it (website, Discord server) so they can compare it.
3. For each admin: they run `vgames keys issue-publisher`; you add their public key with
   `vgames trust build` / `vgames trust sign` and upload the bundle in the admin UI (Trust).
4. Store `root.vgkey` on two offline media. Never copy it to the server.

### Runtime
- Container image from `release-api.yml` (`ghcr.io/<owner>/vgames-api`), behind HTTPS
  (managed load balancer or reverse proxy). WebSockets must be allowed on `/v1/realtime`.
- `VGAMES_PUBLIC_URL` must equal the external HTTPS origin exactly (cookie and CSRF checks use it).

## 2. Project-wide (GitHub)

| Secret / setting | Used by | Notes |
|---|---|---|
| Environment `release` with required reviewers | `release-desktop.yml`, `release-api.yml` | Releases cannot run unattended |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | launcher updater signatures | From `pnpm tauri signer generate`; public key goes in `tauri.conf.json` |
| Windows code signing (e.g. `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`, or a cloud HSM/Trusted Signing config) | release | Authenticode |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | release | Notarization |
| Self-hosted runner (optional) | `soak.yml` | 24 h leak test |
| `ANTHROPIC_API_KEY` (in the `release` environment only) | release-notes agent | Writes the user-facing changelog from agent-written fragments (08-release §3.4) |
| `VGAMES_RUNTIME_CATALOG_KEY`, `VGAMES_RUNTIME_CATALOG_KEY_PASSWORD` | `release-runtimes.yml` | minisign key signing `runtimes.json`; its public key is compiled into the launcher |
| macOS runner (GitHub-hosted is fine) | `runtimes.yml` smoke tests, macOS builds | Wine + DXMT test on Apple Silicon |

## 3. Decisions and requests only humans can make

Recorded decisions (2026-09-24): vgames is **non-commercial**, so D3DMetal is redistributed through the
runtime catalog; the **Steam Linux Runtime** trust boundary (umu downloads it from Valve) is **accepted**.
See 09-compatibility §7.


| Item | Why | Where it's used |
|---|---|---|
| Request Apple's `com.apple.developer.hid.virtual.device` entitlement (Developer portal → identifier → capabilities) | Needed for virtual gamepads on macOS 15+ via CoreHID. Without it, macOS gets passthrough only | 07-controllers §4 |
| When Apple ships a new Game Porting Toolkit: download it with an Apple ID and drop it in the `runtimes-intake` location | Apple's download needs a login; the `runtimes.yml` workflow then verifies Apple's signature and publishes D3DMetal unmodified with its license | 09-compatibility §3, agent-5 A5-T12 |
| Antivirus vendor relations | The overlay's DLL injection can trigger heuristics; signed binaries plus false-positive reports | 05-social §6.4 |

Also replace `REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY` in `apps/desktop/src-tauri/tauri.conf.json`. The updater
and runtime catalog download from `github.com/wouhliss/vgames` releases, so those release assets must be
publicly downloadable (a public repo, or a separate public releases repo with the endpoints updated).
