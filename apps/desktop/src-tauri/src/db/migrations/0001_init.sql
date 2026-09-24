-- Launcher local database, version 1 (A2-T02).
-- Times are Unix seconds (UTC). UUIDs are lowercase hyphenated text.
-- Append-only: never edit a released migration; add a new file instead.

CREATE TABLE servers (
    id                TEXT PRIMARY KEY,               -- server_id from /.well-known/vgames.json
    url               TEXT NOT NULL UNIQUE,           -- normalized base URL
    name              TEXT NOT NULL,
    root_public_key   BLOB NOT NULL CHECK (length(root_public_key) = 32),
    root_fingerprint  TEXT NOT NULL,                  -- VG1-…, pinned (TOFU or deep link)
    trust_version     INTEGER,                        -- highest verified bundle version (rollback guard)
    trust_bundle      BLOB,                           -- exact signed bytes
    trust_signature   BLOB,
    added_at          INTEGER NOT NULL,
    last_connected_at INTEGER,
    is_active         INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1))
);
-- At most one active server.
CREATE UNIQUE INDEX servers_single_active ON servers (is_active) WHERE is_active = 1;

CREATE TABLE accounts (
    server_id   TEXT PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL,
    username    TEXT NOT NULL,
    avatar_url  TEXT,
    role        TEXT NOT NULL CHECK (role IN ('user', 'admin', 'owner')),
    updated_at  INTEGER NOT NULL
);

CREATE TABLE libraries (
    id          TEXT PRIMARY KEY,                     -- also written to <root>/.vgames-library.json
    path        TEXT NOT NULL UNIQUE,                 -- canonical absolute path
    label       TEXT NOT NULL,
    is_default  INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX libraries_single_default ON libraries (is_default) WHERE is_default = 1;

CREATE TABLE installs (
    server_id         TEXT NOT NULL REFERENCES servers (id) ON DELETE RESTRICT,
    package_id        TEXT NOT NULL,
    library_id        TEXT NOT NULL REFERENCES libraries (id) ON DELETE RESTRICT,
    dir_name          TEXT NOT NULL,                  -- <slug> or <slug>-N inside the library
    version_id        TEXT NOT NULL,
    sequence          INTEGER NOT NULL CHECK (sequence > 0),
    platform          TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN
                        ('installing', 'installed', 'updating', 'repairing', 'moving', 'uninstalling', 'broken')),
    installed_at      INTEGER,
    last_played_at    INTEGER,
    playtime_seconds  INTEGER NOT NULL DEFAULT 0 CHECK (playtime_seconds >= 0),
    size_bytes        INTEGER NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    PRIMARY KEY (server_id, package_id),
    UNIQUE (library_id, dir_name)
);

CREATE TABLE favorites (
    server_id   TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id  TEXT NOT NULL,
    position    INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id)
);

CREATE TABLE collections (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    position    INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE collection_items (
    collection_id  TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    server_id      TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id     TEXT NOT NULL,
    position       INTEGER NOT NULL,
    added_at       INTEGER NOT NULL,
    PRIMARY KEY (collection_id, server_id, package_id)
);

-- Typed at the Rust layer (db::settings): value is JSON.
CREATE TABLE settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL CHECK (json_valid(value)),
    updated_at  INTEGER NOT NULL
);

CREATE TABLE download_jobs (
    id          TEXT PRIMARY KEY,
    server_id   TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id  TEXT NOT NULL,
    version_id  TEXT NOT NULL,
    library_id  TEXT NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('install', 'update', 'repair')),
    state       TEXT NOT NULL CHECK (state IN ('queued', 'active', 'paused', 'failed')),
    position    INTEGER NOT NULL,
    options     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options)),
    error       TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    UNIQUE (server_id, package_id)
);

CREATE TABLE save_sync_state (
    server_id         TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id        TEXT NOT NULL,
    base_snapshot_id  TEXT,
    files             TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(files)),  -- root/path -> [size, mtime, blake3]
    sync_pending      INTEGER NOT NULL DEFAULT 0 CHECK (sync_pending IN (0, 1)),
    updated_at        INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id)
);

-- server_id/package_id empty strings = the global default profile.
CREATE TABLE controller_profiles (
    server_id   TEXT NOT NULL DEFAULT '',
    package_id  TEXT NOT NULL DEFAULT '',
    mode        TEXT NOT NULL DEFAULT 'auto' CHECK (mode IN ('auto', 'always', 'never')),
    profile     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(profile)),
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id)
);

CREATE TABLE shortcuts (
    server_id   TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id  TEXT NOT NULL,
    path        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id, path)
);
