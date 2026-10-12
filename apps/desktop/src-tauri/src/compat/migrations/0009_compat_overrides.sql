-- A player's local compatibility overrides per package (GAME-07, 09-compatibility §4).
-- Stored on this computer only; diagnostics mark them as local.
CREATE TABLE compat_overrides (
    server_id      TEXT NOT NULL,
    package_id     TEXT NOT NULL,
    runner_runtime TEXT,
    runner_version TEXT,
    graphics       TEXT,
    -- JSON object of extra environment variables, validated before storing.
    env            TEXT NOT NULL DEFAULT '{}',
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id)
);
