-- vgames initial schema.
-- Requires PostgreSQL 18+ (built-in uuidv7()).
--
-- Conventions (docs/architecture/04-database.md):
--   * Primary keys are UUIDv7 (time-ordered) unless a natural key is better.
--   * Enumerations are `text` + CHECK (easy to extend in later migrations).
--   * All timestamps are timestamptz, stored in UTC.
--   * Secrets (tokens, codes) are stored only as 32-byte SHA-256 digests.
--   * Never edit this file after it is merged: add a new migration instead.

-- ---------------------------------------------------------------------------
-- Helpers
-- ---------------------------------------------------------------------------

CREATE FUNCTION set_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- Server settings
-- ---------------------------------------------------------------------------

CREATE TABLE server_settings (
  key         text PRIMARY KEY CHECK (key ~ '^[a-z][a-z0-9_.]{1,63}$'),
  value       jsonb NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now(),
  updated_by  uuid
);

INSERT INTO server_settings (key, value) VALUES
  ('registration.mode', '"allowlist"'),   -- open | allowlist | closed
  ('server.motd', '""');

-- ---------------------------------------------------------------------------
-- Users, registration and sessions
-- ---------------------------------------------------------------------------

CREATE TABLE users (
  id               uuid PRIMARY KEY DEFAULT uuidv7(),
  discord_id       text NOT NULL UNIQUE CHECK (discord_id ~ '^[0-9]{5,25}$'),
  username         text NOT NULL CHECK (char_length(username) BETWEEN 1 AND 64),
  display_name     text CHECK (char_length(display_name) <= 64),
  avatar_hash      text CHECK (avatar_hash ~ '^(a_)?[0-9a-f]{32}$'),
  role             text NOT NULL DEFAULT 'user' CHECK (role IN ('user', 'admin', 'owner')),
  created_at       timestamptz NOT NULL DEFAULT now(),
  updated_at       timestamptz NOT NULL DEFAULT now(),
  last_seen_at     timestamptz,
  disabled_at      timestamptz,
  disabled_reason  text CHECK (char_length(disabled_reason) <= 500)
);
CREATE INDEX users_username_idx ON users (lower(username));
CREATE TRIGGER users_updated_at BEFORE UPDATE ON users
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE server_settings
  ADD CONSTRAINT server_settings_updated_by_fk FOREIGN KEY (updated_by) REFERENCES users (id) ON DELETE SET NULL;

-- Discord accounts allowed to sign in when registration.mode = 'allowlist'.
CREATE TABLE registration_allowlist (
  discord_id  text PRIMARY KEY CHECK (discord_id ~ '^[0-9]{5,25}$'),
  note        text CHECK (char_length(note) <= 200),
  added_by    uuid REFERENCES users (id) ON DELETE SET NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- A launcher installation. Doubles as the E2EE device (identity keys below).
CREATE TABLE devices (
  id              uuid PRIMARY KEY DEFAULT uuidv7(),
  user_id         uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  display_name    text NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 64),
  platform        text NOT NULL CHECK (platform IN ('windows', 'linux', 'macos')),
  -- vodozemac/Olm account keys, unpadded base64 as produced by vodozemac.
  identity_key    text CHECK (char_length(identity_key) = 43),   -- Curve25519
  signing_key     text CHECK (char_length(signing_key) = 43),    -- Ed25519
  keys_signature  text CHECK (char_length(keys_signature) = 86), -- Ed25519 sig over canonical key JSON
  created_at      timestamptz NOT NULL DEFAULT now(),
  last_seen_at    timestamptz,
  revoked_at      timestamptz,
  CHECK ((identity_key IS NULL) = (signing_key IS NULL) AND (signing_key IS NULL) = (keys_signature IS NULL))
);
CREATE INDEX devices_user_idx ON devices (user_id) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX devices_identity_key_uq ON devices (identity_key) WHERE identity_key IS NOT NULL;

CREATE TABLE sessions (
  id                          uuid PRIMARY KEY DEFAULT uuidv7(),
  user_id                     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  kind                        text NOT NULL CHECK (kind IN ('desktop', 'web')),
  device_id                   uuid REFERENCES devices (id) ON DELETE CASCADE,
  access_token_hash           bytea NOT NULL UNIQUE CHECK (octet_length(access_token_hash) = 32),
  access_expires_at           timestamptz NOT NULL,
  refresh_token_hash          bytea UNIQUE CHECK (octet_length(refresh_token_hash) = 32),
  -- The refresh token that was just rotated out. Presenting it again means the
  -- token leaked: the whole session is revoked (reuse detection).
  previous_refresh_token_hash bytea CHECK (octet_length(previous_refresh_token_hash) = 32),
  refresh_expires_at          timestamptz,
  csrf_token_hash             bytea CHECK (octet_length(csrf_token_hash) = 32),
  user_agent                  text CHECK (char_length(user_agent) <= 512),
  ip                          inet,
  created_at                  timestamptz NOT NULL DEFAULT now(),
  last_used_at                timestamptz NOT NULL DEFAULT now(),
  revoked_at                  timestamptz,
  revoked_reason              text CHECK (revoked_reason IN ('logout', 'refresh_reuse', 'admin', 'user_disabled', 'device_revoked', 'expired')),
  CHECK (kind = 'web' OR refresh_token_hash IS NOT NULL),
  CHECK (kind = 'desktop' OR csrf_token_hash IS NOT NULL)
);
CREATE INDEX sessions_user_active_idx ON sessions (user_id) WHERE revoked_at IS NULL;
CREATE INDEX sessions_prev_refresh_idx ON sessions (previous_refresh_token_hash) WHERE previous_refresh_token_hash IS NOT NULL;

-- In-flight Discord OAuth flows (state parameter). Short-lived.
CREATE TABLE oauth_flows (
  state_hash      bytea PRIMARY KEY CHECK (octet_length(state_hash) = 32),
  client_kind     text NOT NULL CHECK (client_kind IN ('desktop', 'web')),
  -- PKCE S256 challenge from the launcher; binds the login code to the launcher that started the flow.
  code_challenge  text CHECK (code_challenge ~ '^[A-Za-z0-9_-]{43}$'),
  device_name     text CHECK (char_length(device_name) <= 64),
  return_to       text CHECK (return_to ~ '^/admin(/[A-Za-z0-9._~/-]*)?$'),
  created_at      timestamptz NOT NULL DEFAULT now(),
  expires_at      timestamptz NOT NULL,
  consumed_at     timestamptz,
  CHECK (client_kind = 'web' OR code_challenge IS NOT NULL)
);
CREATE INDEX oauth_flows_expiry_idx ON oauth_flows (expires_at);

-- One-time codes handed to the launcher through vgames://auth/callback.
CREATE TABLE login_codes (
  code_hash       bytea PRIMARY KEY CHECK (octet_length(code_hash) = 32),
  user_id         uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  code_challenge  text NOT NULL CHECK (code_challenge ~ '^[A-Za-z0-9_-]{43}$'),
  device_name     text CHECK (char_length(device_name) <= 64),
  created_at      timestamptz NOT NULL DEFAULT now(),
  expires_at      timestamptz NOT NULL,
  consumed_at     timestamptz
);
CREATE INDEX login_codes_expiry_idx ON login_codes (expires_at);

-- ---------------------------------------------------------------------------
-- Trust: publisher keys and root-signed trust bundles
-- ---------------------------------------------------------------------------

-- Raw bundles exactly as signed offline by the root key. Source of truth.
CREATE TABLE trust_bundles (
  version      bigint PRIMARY KEY CHECK (version > 0),
  bundle       bytea NOT NULL CHECK (octet_length(bundle) <= 1048576),
  signature    bytea NOT NULL CHECK (octet_length(signature) = 64),
  expires_at   timestamptz,
  uploaded_by  uuid REFERENCES users (id) ON DELETE SET NULL,
  created_at   timestamptz NOT NULL DEFAULT now()
);

-- Materialized from the latest trust bundle, for fast checks at upload time.
CREATE TABLE publisher_keys (
  key_id             text PRIMARY KEY CHECK (key_id ~ '^[0-9a-f]{32}$'),
  public_key         bytea NOT NULL UNIQUE CHECK (octet_length(public_key) = 32),
  holder_user_id     uuid NOT NULL REFERENCES users (id),
  label              text NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
  not_before         timestamptz NOT NULL,
  not_after          timestamptz NOT NULL,
  trust_version      bigint NOT NULL REFERENCES trust_bundles (version),
  revoked_at         timestamptz,
  revocation_reason  text CHECK (char_length(revocation_reason) <= 500),
  CHECK (not_after > not_before)
);
CREATE INDEX publisher_keys_holder_idx ON publisher_keys (holder_user_id);

-- ---------------------------------------------------------------------------
-- Packages
-- ---------------------------------------------------------------------------

CREATE TABLE packages (
  id                uuid PRIMARY KEY DEFAULT uuidv7(),
  slug              text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$'),
  title             text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
  summary           text CHECK (char_length(summary) <= 500),
  -- Plain text / CommonMark subset. HTML from providers is converted before storage.
  description       text CHECK (char_length(description) <= 20000),
  developer         text CHECK (char_length(developer) <= 200),
  publisher         text CHECK (char_length(publisher) <= 200),
  release_date      date,
  genres            text[] NOT NULL DEFAULT '{}' CHECK (cardinality(genres) <= 20),
  steam_app_id      bigint CHECK (steam_app_id > 0),
  igdb_id           bigint CHECK (igdb_id > 0),
  cover_asset_id    uuid,
  hero_asset_id     uuid,
  logo_asset_id     uuid,
  -- Which source filled each field: {"title": "admin", "summary": "igdb", ...}.
  -- Metadata refreshes never overwrite a field whose source is "admin".
  field_sources     jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(field_sources) = 'object'),
  -- Informational only (fetched from ProtonDB when steam_app_id is known); never used for execution.
  protondb_tier     text CHECK (protondb_tier IN ('platinum', 'gold', 'silver', 'bronze', 'borked', 'pending')),
  status            text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'hidden', 'archived')),
  created_by        uuid NOT NULL REFERENCES users (id),
  created_at        timestamptz NOT NULL DEFAULT now(),
  updated_at        timestamptz NOT NULL DEFAULT now(),
  deleted_at        timestamptz
);
CREATE INDEX packages_status_idx ON packages (status, lower(title)) WHERE deleted_at IS NULL;
CREATE INDEX packages_steam_idx ON packages (steam_app_id) WHERE steam_app_id IS NOT NULL;
CREATE INDEX packages_igdb_idx ON packages (igdb_id) WHERE igdb_id IS NOT NULL;
CREATE TRIGGER packages_updated_at BEFORE UPDATE ON packages
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE package_assets (
  id            uuid PRIMARY KEY DEFAULT uuidv7(),
  package_id    uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  kind          text NOT NULL CHECK (kind IN ('cover', 'hero', 'logo', 'screenshot', 'icon')),
  object_name   text NOT NULL UNIQUE,
  content_type  text NOT NULL CHECK (content_type IN ('image/jpeg', 'image/png', 'image/webp')),
  width         integer NOT NULL CHECK (width BETWEEN 1 AND 16384),
  height        integer NOT NULL CHECK (height BETWEEN 1 AND 16384),
  byte_size     integer NOT NULL CHECK (byte_size BETWEEN 1 AND 10485760),
  sha256        bytea NOT NULL CHECK (octet_length(sha256) = 32),
  source        text NOT NULL CHECK (source IN ('igdb', 'steam', 'upload')),
  source_url    text,
  position      integer NOT NULL DEFAULT 0,
  created_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (package_id, sha256)
);
CREATE INDEX package_assets_package_idx ON package_assets (package_id, kind, position);

ALTER TABLE packages
  ADD CONSTRAINT packages_cover_fk FOREIGN KEY (cover_asset_id) REFERENCES package_assets (id) ON DELETE SET NULL,
  ADD CONSTRAINT packages_hero_fk  FOREIGN KEY (hero_asset_id)  REFERENCES package_assets (id) ON DELETE SET NULL,
  ADD CONSTRAINT packages_logo_fk  FOREIGN KEY (logo_asset_id)  REFERENCES package_assets (id) ON DELETE SET NULL;

-- Candidates returned by IGDB/Steam for an admin to pick from.
CREATE TABLE metadata_candidates (
  package_id    uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  source        text NOT NULL CHECK (source IN ('igdb', 'steam')),
  external_id   bigint NOT NULL CHECK (external_id > 0),
  title         text NOT NULL,
  release_year  smallint,
  score         real NOT NULL DEFAULT 0,   -- title similarity, 0..1
  data          jsonb NOT NULL,            -- normalized fields, see docs/architecture/03-api.md
  fetched_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (package_id, source, external_id)
);

CREATE TABLE package_versions (
  id                 uuid PRIMARY KEY DEFAULT uuidv7(),
  package_id         uuid NOT NULL REFERENCES packages (id) ON DELETE RESTRICT,
  platform           text NOT NULL CHECK (platform IN ('windows-x86_64', 'windows-aarch64', 'linux-x86_64', 'linux-aarch64', 'macos-aarch64', 'macos-x86_64')),
  -- Server-assigned, strictly increasing per (package, platform). Signed into
  -- the manifest; clients refuse to "update" to a lower sequence (rollback).
  sequence           bigint NOT NULL CHECK (sequence > 0),
  version_label      text NOT NULL CHECK (char_length(version_label) BETWEEN 1 AND 64),
  state              text NOT NULL DEFAULT 'uploading'
                     CHECK (state IN ('uploading', 'verifying', 'ready', 'published', 'failed', 'yanked', 'aborted')),
  failure_reason     text CHECK (char_length(failure_reason) <= 2000),
  -- Known only at finalize: per-chunk compression makes pack boundaries data-dependent.
  pack_count         integer CHECK (pack_count BETWEEN 1 AND 100000),
  total_size         bigint CHECK (total_size >= 0),
  file_count         integer CHECK (file_count >= 0),
  chunk_count        integer CHECK (chunk_count >= 0),
  manifest_object    text,
  manifest_size      bigint CHECK (manifest_size BETWEEN 1 AND 268435456),
  manifest_blake3    bytea CHECK (octet_length(manifest_blake3) = 32),
  signature          bytea CHECK (octet_length(signature) = 64),
  publisher_key_id   text REFERENCES publisher_keys (key_id),
  created_by         uuid NOT NULL REFERENCES users (id),
  created_at         timestamptz NOT NULL DEFAULT now(),
  finalized_at       timestamptz,
  verified_at        timestamptz,
  published_at       timestamptz,
  yanked_at          timestamptz,
  UNIQUE (package_id, platform, sequence),
  -- Once past 'uploading', a version must carry a complete, signed manifest.
  CHECK (state IN ('uploading', 'aborted') OR (
    manifest_object IS NOT NULL AND manifest_size IS NOT NULL AND manifest_blake3 IS NOT NULL
    AND signature IS NOT NULL AND publisher_key_id IS NOT NULL AND total_size IS NOT NULL
    AND pack_count IS NOT NULL AND file_count IS NOT NULL AND chunk_count IS NOT NULL
  ))
);
CREATE INDEX package_versions_package_idx ON package_versions (package_id, platform, sequence DESC);

CREATE TABLE package_packs (
  version_id   uuid NOT NULL REFERENCES package_versions (id) ON DELETE CASCADE,
  pack_index   integer NOT NULL CHECK (pack_index >= 0),
  object_name  text NOT NULL UNIQUE,
  size         bigint CHECK (size BETWEEN 1 AND 268435456),
  blake3       bytea CHECK (octet_length(blake3) = 32),
  uploaded_at  timestamptz,
  PRIMARY KEY (version_id, pack_index)
);

-- The version clients get for (package, platform). One row per platform.
CREATE TABLE package_releases (
  package_id  uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  platform    text NOT NULL,
  version_id  uuid NOT NULL UNIQUE REFERENCES package_versions (id),
  updated_by  uuid NOT NULL REFERENCES users (id),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (package_id, platform)
);

-- Signed compatibility profiles (docs/architecture/09-compatibility.md §4): how a Windows build runs
-- on Linux (Proton) or macOS (Wine). Stored verbatim; the launcher verifies the publisher signature.
CREATE TABLE package_compat_profiles (
  package_id        uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  target            text NOT NULL CHECK (target IN ('linux', 'macos')),
  revision          bigint NOT NULL CHECK (revision > 0),
  document          bytea NOT NULL CHECK (octet_length(document) BETWEEN 1 AND 65536),
  signature         bytea NOT NULL CHECK (octet_length(signature) = 64),
  publisher_key_id  text NOT NULL REFERENCES publisher_keys (key_id),
  status            text NOT NULL CHECK (status IN ('verified', 'playable', 'unsupported', 'untested')),
  created_by        uuid NOT NULL REFERENCES users (id),
  created_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (package_id, target, revision)
);

-- Clients report chunk hash mismatches; repeated reports flag a corrupt pack.
CREATE TABLE integrity_reports (
  id           uuid PRIMARY KEY DEFAULT uuidv7(),
  version_id   uuid NOT NULL REFERENCES package_versions (id) ON DELETE CASCADE,
  pack_index   integer NOT NULL CHECK (pack_index >= 0),
  chunk_index  integer CHECK (chunk_index >= 0),
  user_id      uuid REFERENCES users (id) ON DELETE SET NULL,
  detail       text CHECK (char_length(detail) <= 1000),
  created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX integrity_reports_version_idx ON integrity_reports (version_id, pack_index);

-- ---------------------------------------------------------------------------
-- Cloud saves (content-addressed per user)
-- ---------------------------------------------------------------------------

CREATE TABLE save_blobs (
  user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  blake3       bytea NOT NULL CHECK (octet_length(blake3) = 32),
  size         bigint NOT NULL CHECK (size BETWEEN 0 AND 4294967296),
  object_name  text NOT NULL UNIQUE,
  created_at   timestamptz NOT NULL DEFAULT now(),
  uploaded_at  timestamptz,           -- NULL until the upload is confirmed
  PRIMARY KEY (user_id, blake3)
);

CREATE TABLE save_snapshots (
  id           uuid PRIMARY KEY DEFAULT uuidv7(),
  user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  package_id   uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  parent_id    uuid REFERENCES save_snapshots (id) ON DELETE SET NULL,
  device_id    uuid REFERENCES devices (id) ON DELETE SET NULL,
  platform     text NOT NULL CHECK (platform IN ('windows', 'linux', 'macos')),
  file_count   integer NOT NULL CHECK (file_count BETWEEN 0 AND 100000),
  total_size   bigint NOT NULL CHECK (total_size >= 0),
  -- [{"root": "<save root id>", "path": "rel/path", "size": 1, "blake3": "<hex>", "mtime": "<rfc3339>"}]
  files        jsonb NOT NULL CHECK (jsonb_typeof(files) = 'array'),
  label        text CHECK (char_length(label) <= 100),
  created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX save_snapshots_user_pkg_idx ON save_snapshots (user_id, package_id, created_at DESC);

-- Blob references per snapshot, for garbage collection.
CREATE TABLE save_snapshot_blobs (
  snapshot_id  uuid NOT NULL REFERENCES save_snapshots (id) ON DELETE CASCADE,
  user_id      uuid NOT NULL,
  blake3       bytea NOT NULL,
  PRIMARY KEY (snapshot_id, blake3),
  FOREIGN KEY (user_id, blake3) REFERENCES save_blobs (user_id, blake3) ON DELETE RESTRICT
);
CREATE INDEX save_snapshot_blobs_blob_idx ON save_snapshot_blobs (user_id, blake3);

-- Current head per (user, package). Updated with compare-and-swap on parent id.
CREATE TABLE save_heads (
  user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  package_id   uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  snapshot_id  uuid NOT NULL REFERENCES save_snapshots (id),
  updated_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, package_id)
);

-- ---------------------------------------------------------------------------
-- Social: friends, blocks, friend codes, presence
-- ---------------------------------------------------------------------------

CREATE TABLE friendships (
  user_low      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  user_high     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  state         text NOT NULL CHECK (state IN ('pending', 'accepted')),
  requested_by  uuid NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  accepted_at   timestamptz,
  PRIMARY KEY (user_low, user_high),
  CHECK (user_low < user_high),
  CHECK (requested_by IN (user_low, user_high)),
  CHECK ((state = 'accepted') = (accepted_at IS NOT NULL))
);
CREATE INDEX friendships_high_idx ON friendships (user_high);

CREATE TABLE user_blocks (
  blocker_id  uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  blocked_id  uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (blocker_id, blocked_id),
  CHECK (blocker_id <> blocked_id)
);
CREATE INDEX user_blocks_blocked_idx ON user_blocks (blocked_id);

-- Short-lived codes to add a friend (Arachnel-style), 8 chars of Crockford base32.
CREATE TABLE friend_codes (
  code        text PRIMARY KEY CHECK (code ~ '^[0-9A-HJKMNP-TV-Z]{8}$'),
  user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at  timestamptz NOT NULL DEFAULT now(),
  expires_at  timestamptz NOT NULL,
  used_at     timestamptz
);
CREATE INDEX friend_codes_user_idx ON friend_codes (user_id);

-- Ephemeral: rebuilt from live connections, so skip WAL.
CREATE UNLOGGED TABLE user_presence (
  user_id      uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  status       text NOT NULL CHECK (status IN ('online', 'away', 'in_game', 'offline')),
  package_id   uuid REFERENCES packages (id) ON DELETE SET NULL,
  instance_id  text NOT NULL,       -- API instance holding the connection
  updated_at   timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- E2EE key directory and message relay (server sees ciphertext only)
-- ---------------------------------------------------------------------------

CREATE TABLE device_one_time_keys (
  device_id          uuid NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  key_id             text NOT NULL CHECK (char_length(key_id) BETWEEN 1 AND 64),
  public_key         text NOT NULL CHECK (char_length(public_key) = 43),
  signature          text NOT NULL CHECK (char_length(signature) = 86),
  is_fallback        boolean NOT NULL DEFAULT false,
  created_at         timestamptz NOT NULL DEFAULT now(),
  claimed_at         timestamptz,
  claimed_by_device  uuid REFERENCES devices (id) ON DELETE SET NULL,
  PRIMARY KEY (device_id, key_id)
);
CREATE INDEX device_otk_available_idx ON device_one_time_keys (device_id, created_at)
  WHERE claimed_at IS NULL AND NOT is_fallback;
CREATE UNIQUE INDEX device_otk_one_fallback_uq ON device_one_time_keys (device_id)
  WHERE is_fallback AND claimed_at IS NULL;

CREATE TABLE conversations (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  kind        text NOT NULL CHECK (kind IN ('direct', 'party')),
  -- 'direct' only: "<user_low>:<user_high>", guarantees one DM per pair.
  direct_key  text UNIQUE,
  created_by  uuid NOT NULL REFERENCES users (id),
  created_at  timestamptz NOT NULL DEFAULT now(),
  CHECK ((kind = 'direct') = (direct_key IS NOT NULL))
);

CREATE TABLE conversation_members (
  conversation_id  uuid NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  user_id          uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  role             text NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'member')),
  joined_at        timestamptz NOT NULL DEFAULT now(),
  left_at          timestamptz,
  PRIMARY KEY (conversation_id, user_id)
);
CREATE INDEX conversation_members_user_idx ON conversation_members (user_id) WHERE left_at IS NULL;

-- One row per recipient device (client-side fan-out). Deleted on ack.
CREATE TABLE message_envelopes (
  id                   uuid PRIMARY KEY DEFAULT uuidv7(),
  conversation_id      uuid NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  sender_user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  sender_device_id     uuid NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  recipient_device_id  uuid NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  client_message_id    uuid NOT NULL,
  algorithm            text NOT NULL CHECK (algorithm IN ('olm.v1')),
  olm_message_type     smallint NOT NULL CHECK (olm_message_type IN (0, 1)),  -- 0 = pre-key, 1 = normal
  ciphertext           bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 1 AND 65536),
  created_at           timestamptz NOT NULL DEFAULT now(),
  expires_at           timestamptz NOT NULL DEFAULT now() + interval '30 days',
  UNIQUE (sender_device_id, client_message_id, recipient_device_id)
);
CREATE INDEX message_envelopes_inbox_idx ON message_envelopes (recipient_device_id, id);
CREATE INDEX message_envelopes_expiry_idx ON message_envelopes (expires_at);

-- ---------------------------------------------------------------------------
-- Game invites (server-mediated handshake; join secrets travel E2EE)
-- ---------------------------------------------------------------------------

CREATE TABLE game_invites (
  id               uuid PRIMARY KEY DEFAULT uuidv7(),
  from_user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  to_user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  package_id       uuid NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
  conversation_id  uuid REFERENCES conversations (id) ON DELETE SET NULL,
  state            text NOT NULL DEFAULT 'pending'
                   CHECK (state IN ('pending', 'accepted', 'installing', 'ready', 'joined', 'declined', 'cancelled', 'expired', 'failed')),
  -- Installing progress reported by the invitee (0..1), throttled client-side.
  progress         real CHECK (progress BETWEEN 0 AND 1),
  message          text CHECK (char_length(message) <= 200),
  created_at       timestamptz NOT NULL DEFAULT now(),
  updated_at       timestamptz NOT NULL DEFAULT now(),
  expires_at       timestamptz NOT NULL,
  CHECK (from_user_id <> to_user_id)
);
CREATE UNIQUE INDEX game_invites_one_active_uq ON game_invites (from_user_id, to_user_id, package_id)
  WHERE state IN ('pending', 'accepted', 'installing', 'ready');
CREATE INDEX game_invites_to_idx ON game_invites (to_user_id, state);
CREATE INDEX game_invites_expiry_idx ON game_invites (expires_at)
  WHERE state IN ('pending', 'accepted', 'installing', 'ready');
CREATE TRIGGER game_invites_updated_at BEFORE UPDATE ON game_invites
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- Operations: background jobs, idempotency, audit
-- ---------------------------------------------------------------------------

-- Postgres-backed queue: workers claim with FOR UPDATE SKIP LOCKED.
CREATE TABLE jobs (
  id            uuid PRIMARY KEY DEFAULT uuidv7(),
  kind          text NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_.]{1,63}$'),
  payload       jsonb NOT NULL DEFAULT '{}',
  state         text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'dead')),
  priority      smallint NOT NULL DEFAULT 0,
  run_at        timestamptz NOT NULL DEFAULT now(),
  attempts      integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  max_attempts  integer NOT NULL DEFAULT 5 CHECK (max_attempts BETWEEN 1 AND 100),
  locked_by     text,
  locked_until  timestamptz,
  last_error    text,
  -- Prevents duplicate queued jobs, e.g. 'metadata.fetch:<package_id>'.
  dedupe_key    text,
  created_at    timestamptz NOT NULL DEFAULT now(),
  updated_at    timestamptz NOT NULL DEFAULT now(),
  finished_at   timestamptz
);
CREATE INDEX jobs_ready_idx ON jobs (priority DESC, run_at) WHERE state = 'queued';
CREATE INDEX jobs_running_idx ON jobs (locked_until) WHERE state = 'running';
CREATE UNIQUE INDEX jobs_dedupe_uq ON jobs (dedupe_key) WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running');
CREATE TRIGGER jobs_updated_at BEFORE UPDATE ON jobs
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE idempotency_keys (
  user_id              uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  key                  text NOT NULL CHECK (key ~ '^[A-Za-z0-9_-]{16,128}$'),
  request_fingerprint  bytea NOT NULL CHECK (octet_length(request_fingerprint) = 32),
  status_code          smallint,
  response_body        jsonb,
  created_at           timestamptz NOT NULL DEFAULT now(),
  expires_at           timestamptz NOT NULL DEFAULT now() + interval '24 hours',
  PRIMARY KEY (user_id, key)
);
CREATE INDEX idempotency_keys_expiry_idx ON idempotency_keys (expires_at);

-- Append-only record of every privileged action.
CREATE TABLE audit_log (
  id             uuid PRIMARY KEY DEFAULT uuidv7(),
  actor_user_id  uuid REFERENCES users (id) ON DELETE SET NULL,
  action         text NOT NULL CHECK (action ~ '^[a-z][a-z0-9_.]{1,63}$'),
  target_type    text,
  target_id      text,
  ip             inet,
  user_agent     text CHECK (char_length(user_agent) <= 512),
  details        jsonb NOT NULL DEFAULT '{}',
  created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_created_idx ON audit_log (created_at DESC);
CREATE INDEX audit_log_actor_idx ON audit_log (actor_user_id, created_at DESC);
CREATE INDEX audit_log_target_idx ON audit_log (target_type, target_id);

-- The only permitted mutation is the FK's ON DELETE SET NULL of actor_user_id.
CREATE FUNCTION audit_log_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE'
     AND OLD.actor_user_id IS NOT NULL AND NEW.actor_user_id IS NULL
     AND (NEW.id, NEW.action, NEW.target_type, NEW.target_id, NEW.ip, NEW.user_agent, NEW.details, NEW.created_at)
         IS NOT DISTINCT FROM
         (OLD.id, OLD.action, OLD.target_type, OLD.target_id, OLD.ip, OLD.user_agent, OLD.details, OLD.created_at)
  THEN
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'audit_log is append-only';
END;
$$;
CREATE TRIGGER audit_log_no_update BEFORE UPDATE OR DELETE ON audit_log
  FOR EACH ROW EXECUTE FUNCTION audit_log_append_only();
