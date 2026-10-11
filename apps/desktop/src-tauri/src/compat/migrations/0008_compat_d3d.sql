-- Direct3D imports of a release's launch executables (GAME-06, 09-compatibility §3),
-- recorded when the priority-files hook or a launch scanned them, so details and
-- retries show the result without downloading again.
CREATE TABLE compat_d3d_scans (
    server_id  TEXT NOT NULL,
    package_id TEXT NOT NULL,
    version_id TEXT NOT NULL,
    d3d9       INTEGER NOT NULL,
    d3d10      INTEGER NOT NULL,
    d3d11      INTEGER NOT NULL,
    d3d12      INTEGER NOT NULL,
    scanned_at INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id, version_id)
);
