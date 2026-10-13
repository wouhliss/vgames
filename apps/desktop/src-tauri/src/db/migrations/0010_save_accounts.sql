-- No legacy row can be assigned to an account whose identity was not recorded.
-- Preserve unknown-account records under an inaccessible empty account key.
ALTER TABLE save_sync_state RENAME TO save_sync_state_unscoped;
CREATE TABLE save_sync_state (
    server_id TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    package_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    base_snapshot_id TEXT,
    files TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(files)),
    sync_pending INTEGER NOT NULL DEFAULT 0 CHECK (sync_pending IN (0, 1)),
    updated_at INTEGER NOT NULL,
    last_synced_at INTEGER,
    push_operation TEXT CHECK (push_operation IS NULL OR json_valid(push_operation)),
    PRIMARY KEY (server_id, package_id, user_id)
);
INSERT INTO save_sync_state
    (server_id, package_id, user_id, base_snapshot_id, files, sync_pending, updated_at)
SELECT server_id, package_id, '', base_snapshot_id, files, 1, updated_at
FROM save_sync_state_unscoped;
DROP TABLE save_sync_state_unscoped;
