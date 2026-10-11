-- Signed compat profiles (GAME-04, 09-compatibility §4): the highest verified
-- revision per (server, package, target), with the exact signed bytes and the
-- signature envelope, verified again whenever they are read back.
CREATE TABLE compat_profiles (
    server_id  TEXT NOT NULL,
    package_id TEXT NOT NULL,
    target     TEXT NOT NULL CHECK (target IN ('linux', 'macos')),
    revision   INTEGER NOT NULL CHECK (revision >= 1),
    document   BLOB NOT NULL,
    envelope   BLOB NOT NULL,
    stored_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, package_id, target)
);
