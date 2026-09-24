# 04 — Database

PostgreSQL **18+**. The authoritative DDL is
[`apps/api/migrations/20260924000001_init.sql`](../../apps/api/migrations/20260924000001_init.sql)
(verified against `postgres:18.6-alpine`). This page explains the model and its invariants.

## 1. Conventions

- sqlx migrations, **append-only**: never edit a merged migration. Name new ones
  `YYYYMMDDHHMMSS_<snake_case>.sql`. Every migration must apply to an empty database
  **and** to a copy of the previous schema with data (CI runs both).
- Keys: `uuid DEFAULT uuidv7()` (time-ordered, index-friendly); natural keys where they are stable (`discord_id`, `key_id`, `(user_id, blake3)`).
- Enumerations: `text` + `CHECK (x IN (…))`. Map to Rust enums with `#[derive(sqlx::Type)] #[sqlx(type_name = "text", rename_all = "snake_case")]`.
- Every column has the tightest `CHECK` that is cheap: lengths, formats, ranges, cross-column rules.
  The database is the last line of defence; handlers validate first and return 400s.
- `updated_at` maintained by the `set_updated_at()` trigger where present.
- Secrets are stored as SHA-256 digests only (`*_hash bytea`, 32 bytes).
- All queries use `sqlx::query!`/`query_as!` (compile-time checked; `.sqlx/` offline data committed).
- Multi-step state changes run in one transaction; the audit row is written in the same transaction.
- State transitions use guarded updates: `UPDATE … SET state = $new WHERE id = $id AND state = ANY($allowed) RETURNING …`.
  Zero rows → `409 conflict`. No read-then-write races.

## 2. Model

```
users ─┬─< sessions >── devices ─┬─< device_one_time_keys
       │                         └─< message_envelopes (recipient_device_id)
       ├─< friendships (user_low,user_high) ; user_blocks ; friend_codes
       ├─< conversation_members >── conversations ─< message_envelopes
       ├─< game_invites (from/to) >── packages
       ├─< save_blobs ─< save_snapshot_blobs >── save_snapshots ──< save_heads (user,package)
       ├─< publisher_keys (holder) ── trust_bundles (version)
       └─< audit_log (actor)

packages ─┬─< package_assets (cover/hero/logo FKs back from packages)
          ├─< metadata_candidates
          ├─< package_compat_profiles (target, revision)
          ├─< package_versions ─┬─< package_packs
          │                     └─< integrity_reports
          └─< package_releases (package, platform) ── package_versions

jobs · idempotency_keys · oauth_flows · login_codes · registration_allowlist · server_settings · user_presence (UNLOGGED)
```

| Area | Tables | Notes |
|---|---|---|
| Identity | `users`, `sessions`, `oauth_flows`, `login_codes`, `registration_allowlist`, `devices` | Discord is the only identity provider. `devices` = launcher installs = E2EE devices. |
| Trust | `trust_bundles`, `publisher_keys` | Bundles are stored verbatim; `publisher_keys` is rebuilt from the newest bundle. |
| Catalog | `packages`, `package_assets`, `metadata_candidates`, `package_versions`, `package_packs`, `package_releases`, `package_compat_profiles`, `integrity_reports` | Packages are soft-deleted (`deleted_at`). Published versions are immutable. Compat profiles are signed and append-only (new revision per change). |
| Saves | `save_blobs`, `save_snapshots`, `save_snapshot_blobs`, `save_heads` | Content-addressed per user; head updated by compare-and-swap. |
| Social | `friendships`, `user_blocks`, `friend_codes`, `user_presence`, `conversations`, `conversation_members`, `message_envelopes`, `device_one_time_keys`, `game_invites` | Ciphertext only. |
| Ops | `jobs`, `idempotency_keys`, `audit_log`, `server_settings` | `audit_log` is append-only (trigger). |

## 3. State machines

**package_versions.state**

```
uploading ──finalize ok──▶ verifying ──job ok──▶ ready ──publish──▶ published ──yank──▶ yanked
    │                          │
    └──abort──▶ aborted        └──job fails──▶ failed
```

**game_invites.state** (05-social §5)

```
pending ──accept──▶ accepted ──▶ installing ──▶ ready ──▶ joined
   │                   │  └────────────────────▶ ready
   ├─decline─▶ declined  (any active) ──cancel (sender)──▶ cancelled
   └─(time)──▶ expired   (any active) ──(time)──▶ expired ; installing ──error──▶ failed
```

**friendships.state:** `pending` → `accepted`. Decline or remove deletes the row. Blocking
deletes the friendship and prevents new requests in both directions.

## 4. Background jobs (Postgres queue)

Claim: `UPDATE jobs SET state='running', locked_by=$w, locked_until=now()+$lease, attempts=attempts+1
WHERE id = (SELECT id FROM jobs WHERE state='queued' AND run_at<=now() ORDER BY priority DESC, run_at
FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING *`. A reaper requeues `running` jobs past
`locked_until`. Failures back off exponentially: `run_at = now() + 2^attempts × 10 s`.
After `max_attempts` a job becomes `dead` (visible in the admin UI with a retry button).

| kind | Trigger | Does |
|---|---|---|
| `metadata.fetch` | package created / refresh | IGDB + Steam candidates, auto-apply rule |
| `metadata.images` | candidate applied | Download + validate + store images |
| `version.verify` | finalize | Re-hash every pack/chunk from storage |
| `version.cleanup` | abort / failed + 7 days | Delete objects |
| `saves.gc` | daily | Delete snapshots beyond retention, unreferenced blobs |
| `sweep.expired` | every minute | Expire invites, oauth flows, login codes, friend codes, idempotency keys, envelopes |

## 5. Retention

| Data | Retention |
|---|---|
| Undelivered message envelopes | 30 days |
| Sessions | Deleted 30 days after revocation or expiry |
| OAuth flows, login codes | Deleted 1 day after expiry |
| Save snapshots | Latest 20 per (user, package) + any referenced by head |
| Audit log | Forever (append-only) |
| Integrity reports | 180 days |
