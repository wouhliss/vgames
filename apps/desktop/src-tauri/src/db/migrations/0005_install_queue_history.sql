-- INS-03: what the queue and the library show without the server, and the
-- queue history (finished jobs, newest first, at most 100 kept by the code).

-- Catalog data recorded when an install starts (library tiles, queue rows).
ALTER TABLE installs ADD COLUMN title TEXT NOT NULL DEFAULT '';
ALTER TABLE installs ADD COLUMN slug TEXT NOT NULL DEFAULT '';
ALTER TABLE installs ADD COLUMN version_label TEXT NOT NULL DEFAULT '';
ALTER TABLE installs ADD COLUMN cover_asset_id TEXT;

-- Last known progress of a job (live progress comes from events).
ALTER TABLE download_jobs ADD COLUMN bytes_done INTEGER NOT NULL DEFAULT 0 CHECK (bytes_done >= 0);
ALTER TABLE download_jobs ADD COLUMN bytes_total INTEGER NOT NULL DEFAULT 0 CHECK (bytes_total >= 0);

CREATE TABLE download_history (
    id             TEXT PRIMARY KEY,
    server_id      TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id     TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('install', 'update', 'repair')),
    title          TEXT NOT NULL,
    version_label  TEXT NOT NULL,
    bytes_total    INTEGER NOT NULL CHECK (bytes_total >= 0),
    finished_at    INTEGER NOT NULL,
    outcome        TEXT NOT NULL CHECK (json_valid(outcome))
);

CREATE INDEX download_history_finished ON download_history (finished_at DESC, id DESC);
