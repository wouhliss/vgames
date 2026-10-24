-- Durable cloud-save conflicts (06-cloud-saves-notes "Commands and summaries").
-- At most one open conflict per server, package and account. `cloud` is the
-- server's head summary when detected; `device_backup_id` protects the device's
-- files as they were, and retention never removes it while the conflict is open;
-- a resolved conflict's record goes when its backup ages out.
CREATE TABLE save_conflicts (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    head_snapshot_id TEXT NOT NULL,
    cloud TEXT NOT NULL CHECK (json_valid(cloud)),
    device_backup_id TEXT NOT NULL REFERENCES save_backups (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
);
CREATE UNIQUE INDEX save_conflicts_open
    ON save_conflicts (server_id, package_id, user_id) WHERE resolved_at IS NULL;
