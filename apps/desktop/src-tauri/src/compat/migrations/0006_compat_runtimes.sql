-- Compatibility runtimes (GAME-03, 09-compatibility §5).

-- The highest runtime catalog accepted per signing key (F2: never one global
-- version). The verified bytes and signature are kept so the launcher can
-- re-verify and use the catalog offline.
CREATE TABLE compat_catalogs (
    key_id      TEXT PRIMARY KEY,
    version     INTEGER NOT NULL CHECK (version >= 1),
    catalog     BLOB NOT NULL,
    signature   TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    -- Last successful check against the network (refresh at most every 6 h).
    checked_at  INTEGER NOT NULL
);

-- Runtimes extracted under <data_dir>/runtimes/<runtime_id>/<version>/.
CREATE TABLE compat_runtimes (
    runtime_id   TEXT NOT NULL,
    version      TEXT NOT NULL,
    arch         TEXT NOT NULL,
    sha256       TEXT NOT NULL,
    size         INTEGER NOT NULL,
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (runtime_id, version)
);

-- Which installed package uses which runtime version; garbage collection keeps
-- every referenced version.
CREATE TABLE compat_runtime_refs (
    server_id  TEXT NOT NULL,
    package_id TEXT NOT NULL,
    runtime_id TEXT NOT NULL,
    version    TEXT NOT NULL,
    PRIMARY KEY (server_id, package_id, runtime_id)
);
CREATE INDEX compat_runtime_refs_runtime ON compat_runtime_refs (runtime_id, version);
