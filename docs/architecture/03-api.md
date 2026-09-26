# 03 — API Conventions, Endpoint Map and Realtime Protocol

The machine-readable contract is [`openapi/openapi.yaml`](../../openapi/openapi.yaml)
(OpenAPI 3.1). This document explains the rules behind it. When the two disagree,
fix the disagreement in a `contract:` PR. Do not code around it.

## 1. General rules

- Base path `/v1`. Discovery document at `/.well-known/vgames.json` (unversioned, stable forever).
- JSON only (`application/json; charset=utf-8`), **snake_case** fields, UTF-8.
- IDs: UUIDv7 strings. Time: RFC 3339 UTC (`2026-09-24T10:00:00Z`). Sizes: integer bytes.
  Hashes: lowercase hex. Binary: standard base64 with padding.
- Absent optional fields are omitted, not `null`, unless `null` carries meaning (documented per field).
- Unknown request fields are **rejected** (`400 unknown_field`), so typos cannot silently pass.
- Methods: `GET` safe, `POST` create/action, `PATCH` partial update (JSON Merge Patch
  semantics), `PUT` full replace, `DELETE` remove. Actions on a resource use a sub-path verb:
  `POST /v1/admin/versions/{id}/publish`.
- `POST` endpoints that create things accept an `Idempotency-Key` header (16–128 chars
  `[A-Za-z0-9_-]`). A replay with the same key and body returns the original response;
  the same key with a different body returns `409 idempotency_key_reused`.
- Optimistic concurrency on mutable admin resources: responses carry `ETag`; `PATCH`
  requires `If-Match` → `412 precondition_failed` on mismatch.
- Auth: `Authorization: Bearer vga_…` (launcher) or session cookie + `X-CSRF-Token` (admin web).
- Every response carries `X-Request-Id` (echoed if the client sent a valid one).
- Deprecation: `Deprecation` and `Sunset` headers; nothing is removed from `/v1` without a `/v2`.

## 2. Errors — RFC 9457 Problem Details

```http
HTTP/1.1 409 Conflict
Content-Type: application/problem+json

{
  "type": "urn:vgames:problem:save_head_conflict",
  "title": "Cloud save changed on another device",
  "status": 409,
  "code": "save_head_conflict",
  "detail": "Expected head 0192… but the current head is 0193….",
  "instance": "/v1/saves/0192…/snapshots",
  "request_id": "01J…",
  "errors": [ { "field": "parent_snapshot_id", "code": "stale", "message": "…" } ]
}
```

- `type` is `urn:vgames:problem:<code>`. `code` is the stable machine identifier clients switch on. `title` and `detail` are human text.
- `errors[]` is present for validation failures (`400 validation_failed`), one entry per field (JSON Pointer-ish dotted path).
- Never leak internals (SQL, stack traces, bucket names) in `detail`. Log them with the request id.

Common codes: `unauthenticated` 401, `session_expired` 401, `forbidden` 403, `not_found` 404,
`validation_failed` 400, `unknown_field` 400, `conflict` 409, `precondition_failed` 412,
`payload_too_large` 413, `unsupported_media_type` 415, `rate_limited` 429,
`registration_closed` 403, `not_allowlisted` 403, `user_disabled` 403, `internal` 500, `unavailable` 503.
The Discord callback does not answer these as problems: it redirects with `error=<code>` (01-security §4.1).

## 3. Pagination and filtering

Cursor-based, stable under inserts:

```
GET /v1/packages?limit=50&cursor=<opaque>&q=portal&sort=title
→ { "items": [ … ], "next_cursor": "<opaque or absent>" }
```

`limit` defaults to 50, maximum 200. Cursors are opaque base64url, and the server rejects a
cursor that was built with different filters (`400 invalid_cursor`). No offset pagination.

## 4. Endpoint map

Legend: 🔓 public · 👤 user · 🛡 admin · 👑 owner.

### Discovery, health, docs
| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/.well-known/vgames.json` | 🔓 | Server id, name, API version, root public key + fingerprint, features, min launcher version |
| GET | `/v1/health` | 🔓 | Liveness + DB readiness (`{status, db, storage}`) |
| GET | `/openapi.json`, `/docs` | 🔓 | Generated OpenAPI document, Swagger UI |

### Auth and account
| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/auth/discord/start` | 🔓 | Begin Discord login (desktop: PKCE; web: return_to) |
| GET | `/v1/auth/discord/callback` | 🔓 | Discord redirect target (302 to `vgames://…` or `/admin/…`; a refused sign-in redirects with `error=<code>`) |
| POST | `/v1/auth/token` | 🔓 | Exchange login code (+ verifier) or refresh token |
| POST | `/v1/auth/logout` | 👤 | Revoke current session |
| GET | `/v1/me` | 👤 | Current user, role, device |
| GET | `/v1/me/sessions` · DELETE `/v1/me/sessions/{id}` | 👤 | List/revoke own sessions |

### Catalog, downloads, trust
| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/v1/trust/bundle` | 🔓 | Latest signed trust bundle |
| GET | `/v1/packages` | 👤 | Browse published packages (search, genre, platform) |
| GET | `/v1/genres` | 👤 | Genres of the published catalog with counts (≤ 60 s stale) |
| GET | `/v1/packages/{package_id}` | 👤 | Package details + assets + available platforms |
| GET | `/v1/packages/{package_id}/releases/{platform}` | 👤 | Current release descriptor (manifest URL + signature envelope) |
| POST | `/v1/versions/{version_id}/download-urls` | 👤 | Signed GET URLs for up to 500 packs (`410 version_yanked` for withdrawn versions) |
| POST | `/v1/versions/{version_id}/integrity-reports` | 👤 | Report a chunk hash mismatch |
| GET | `/v1/assets/{asset_id}` | 👤 | 302 to a signed image URL (cacheable 1 h) |
| GET | `/v1/packages/{package_id}/compat` | 👤 | Latest signed compat profile per target (Proton on Linux, Wine on macOS) |

### Cloud saves (06-cloud-saves.md)
| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/v1/saves/{package_id}/head` | 👤 | Current head snapshot (or 404 `no_saves`) |
| GET | `/v1/saves/{package_id}/snapshots` | 👤 | History (paginated) |
| GET | `/v1/saves/{package_id}/snapshots/{snapshot_id}` | 👤 | Snapshot with file list |
| POST | `/v1/saves/{package_id}/blobs/prepare` | 👤 | Which blobs are missing + signed PUT URLs |
| POST | `/v1/saves/{package_id}/blobs/download-urls` | 👤 | Signed GET URLs for blobs |
| POST | `/v1/saves/{package_id}/snapshots` | 👤 | Commit a snapshot (compare-and-swap on parent → 409) |

### Social (Agent 4 — 05-social.md)
| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/v1/friends` | 👤 | Friends + pending requests (in/out) with presence |
| POST | `/v1/friends/requests` | 👤 | Send request by `user_id` or `friend_code` |
| POST | `/v1/friends/{user_id}/accept` · `/decline` | 👤 | Answer a request |
| DELETE | `/v1/friends/{user_id}` | 👤 | Remove friend / cancel request |
| POST | `/v1/friend-codes` | 👤 | Create an 8-char code (15 min, single use) |
| POST/DELETE | `/v1/blocks/{user_id}` | 👤 | Block / unblock |
| GET | `/v1/users/{user_id}` | 👤 | Public profile (only friends, pending requests, or shared conversations) |
| PUT | `/v1/presence` | 👤 | Set status / current package (also possible over realtime) |

### E2EE devices and messaging (Agent 4)
| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/devices` | 👤 | Register this launcher's device + identity keys |
| GET | `/v1/devices` · DELETE `/v1/devices/{device_id}` | 👤 | Own devices / revoke one |
| POST | `/v1/devices/{device_id}/one-time-keys` | 👤 | Upload signed one-time keys + fallback key |
| GET | `/v1/users/{user_id}/devices` | 👤 | A contact's device public keys |
| POST | `/v1/keys/claim` | 👤 | Claim one one-time key per target device (atomic) |
| GET | `/v1/conversations` · POST `/v1/conversations` | 👤 | List / create direct or party conversation |
| POST | `/v1/conversations/{conversation_id}/messages` | 👤 | Send ciphertext envelopes (one per device) |
| GET | `/v1/inbox` | 👤 | Envelopes for the calling device (cursor) |
| POST | `/v1/inbox/ack` | 👤 | Acknowledge (delete) delivered envelopes |

### Invites (Agent 4)
| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/invites` | 👤 | Invite a friend to a package |
| GET | `/v1/invites` | 👤 | Active invites (sent and received) |
| POST | `/v1/invites/{invite_id}/accept` · `/decline` · `/cancel` | 👤 | Answer / withdraw |
| POST | `/v1/invites/{invite_id}/status` | 👤 | Invitee reports `installing` (+progress), `ready`, `joined`, `failed` |

### Realtime
| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/realtime/ticket` | 👤 | One-time ticket (30 s) for the WebSocket |
| GET | `/v1/realtime?ticket=…` | ticket | WebSocket upgrade |

### Admin (🛡 unless marked 👑)
| Method | Path | Purpose |
|---|---|---|
| GET/POST | `/v1/admin/packages` | List (all statuses) / create package (enqueues metadata fetch) |
| GET/PATCH/DELETE | `/v1/admin/packages/{package_id}` | Read / edit (If-Match) / soft-delete |
| POST | `/v1/admin/packages/{package_id}/metadata/refresh` | Re-query IGDB + Steam |
| GET | `/v1/admin/packages/{package_id}/metadata/candidates` | Candidates to choose from |
| POST | `/v1/admin/packages/{package_id}/metadata/apply` | Apply a candidate (chosen fields; images downloaded) |
| POST | `/v1/admin/packages/{package_id}/assets` | Upload a custom image (multipart, ≤ 10 MiB) |
| DELETE | `/v1/admin/assets/{asset_id}` | Remove an image |
| PUT | `/v1/admin/packages/{package_id}/compat/{target}` | Publish a new signed compat profile revision |
| GET/POST | `/v1/admin/packages/{package_id}/versions` | List / create version (returns id + sequence) |
| GET/DELETE | `/v1/admin/versions/{version_id}` | Status / abort (unpublished only) |
| POST | `/v1/admin/versions/{version_id}/packs/{pack_index}/upload-session` | Signed resumable-upload start URL |
| POST | `/v1/admin/versions/{version_id}/manifest-upload` | Signed PUT URL for manifest.json |
| POST | `/v1/admin/versions/{version_id}/finalize` | Verify signature + manifest, start verification job |
| POST | `/v1/admin/versions/{version_id}/publish` · `/yank` | Make current / withdraw |
| POST | `/v1/admin/versions/{version_id}/signature` | Replace the signature envelope (re-sign after revocation; manifest bytes unchanged) |
| GET | `/v1/admin/users` · PATCH `/v1/admin/users/{user_id}` | List / disable, enable; role changes 👑 |
| GET/POST/DELETE | `/v1/admin/allowlist[/{discord_id}]` | Registration allowlist |
| GET/PATCH | `/v1/admin/settings` | 👑 Server settings (registration mode, name, MOTD) |
| GET | `/v1/admin/trust/publisher-keys` | Keys from the current bundle + holder + expiry |
| POST | `/v1/admin/trust/bundles` | 👑 Upload a new root-signed bundle |
| GET | `/v1/admin/jobs` · POST `/v1/admin/jobs/{job_id}/retry` | Job queue inspection / retry dead jobs |
| GET | `/v1/admin/audit-log` | Audit trail (filter by actor, action, target, time) |

## 5. Metadata fetching (server side)

On package creation (`title`, optional `steam_app_id` / `igdb_id`), job `metadata.fetch`:

1. **IGDB** (Twitch client-credentials token, cached until expiry): search by title or fetch by id.
   Read name, summary, first_release_date, genres, involved companies (developer/publisher),
   cover, artworks and screenshots `image_id`s, and external ids (Steam).
2. **Steam Store** (`store.steampowered.com/api/storesearch` and `/api/appdetails`): name, short
   description, header/capsule images, screenshots, release date, developers/publishers, genres.
   With a Steam app id, also the ProtonDB summary tier (informational) and the umu-database id
   (a hint shown to admins writing compat profiles).
3. Normalize both into `metadata_candidates.data`:
   `{title, summary, description, release_date, developer, publisher, genres[], images:{cover, hero, logo, screenshots[]}, external:{steam_app_id, igdb_id, umu_id}}`.
   `umu_id` is set on the Steam candidate for the package's Steam app (the admin UI shows it as a compat-profile hint).
4. Auto-apply **only** if an explicit id was given or exactly one candidate has a
   normalized-title similarity ≥ 0.95. Fill only fields whose `field_sources` is not
   `admin`. Otherwise leave the candidates for the admin to pick.
5. Download chosen images server-side under the SSRF rules (01-security §5), store them in the assets
   bucket, and create `package_assets`.

`size` shown to users is always the real `total_size` of the published version, never scraped.
Provider outages mark the job failed with retry (exponential, 5 attempts) and never block package creation.

## 6. Realtime protocol (WebSocket)

Purpose: low-latency **nudges** and ephemeral presence. **REST remains the source of
truth**: after any (re)connect the client re-fetches friends, invites and inbox. Losing a
realtime event can delay things but never loses data.

- Connect: `POST /v1/realtime/ticket` → `GET /v1/realtime?ticket=…` (ticket single-use, 30 s).
- Frames: UTF-8 JSON text, ≤ 64 KiB. Server pings every 25 s; client closes and reconnects if
  there has been no traffic for 60 s. Reconnect with exponential backoff (1 s → 60 s, full jitter).
- Envelope:

```json
{ "v": 1, "id": "0192…", "type": "invite.created", "ts": "2026-09-24T10:00:00Z", "data": { … } }
```

Server → client events:

| type | data | Consumer action |
|---|---|---|
| `hello` | `{user_id, device_id, server_time}` | Resync state via REST |
| `presence.changed` | `{user_id, status, package_id?}` | Update friend list |
| `friend.request` / `friend.accepted` / `friend.removed` | `{user_id}` | Refetch `/v1/friends` |
| `inbox.new` | `{conversation_id, count}` | `GET /v1/inbox` |
| `invite.created` / `invite.updated` | `{invite}` (full object) | Update invite UI / overlay card |
| `device.added` / `device.revoked` | `{user_id, device_id}` | Refresh device list; warn on new contact device |
| `version.state` (admins only) | `{version_id, state, failure_reason?}` | Admin upload progress |
| `session.revoked` | `{reason}` | Sign out locally |

Client → server events: `presence.set {status, package_id?}`, `typing {conversation_id}`
(fire-and-forget, rate-limited), `pong`.

Fan-out across API instances: after committing a change, the handler calls
`pg_notify('vgames_events', json)` with `{recipients: [user_id…], event}` (≤ 7.5 KB; larger events
send `{recipients, ref}` and receivers load by reference). Each instance `LISTEN`s and
forwards to its local sockets.
