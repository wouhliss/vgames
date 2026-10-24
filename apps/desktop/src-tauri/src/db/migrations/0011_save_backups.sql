-- Durable local save backups and restore recovery records (PLAY-06).
-- `id` is the UUIDv7 backup folder under <app data>/save-backups/<server>/<package>/<account>/.
-- A row in state 'restoring' means files may be half-way between two snapshots: startup
-- rolls it back from its backup before any save file is used.
CREATE TABLE save_backups (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('restore', 'conflict')),
    state TEXT NOT NULL CHECK (state IN ('restoring', 'complete', 'rolled_back')),
    file_count INTEGER NOT NULL CHECK (file_count >= 0),
    total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
    created_at INTEGER NOT NULL
);
CREATE INDEX save_backups_scope ON save_backups (server_id, package_id, user_id, id);
CREATE INDEX save_backups_unfinished ON save_backups (state) WHERE state = 'restoring';
