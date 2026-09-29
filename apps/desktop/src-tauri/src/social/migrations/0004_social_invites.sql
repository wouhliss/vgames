-- Local state of game invites (A4-T09, 05-social §5). Owner: Agent 4. Append-only.
-- The server holds the invite itself; this table keeps what only this install knows:
-- the join secret the sender typed (sealed with the keychain chat key; never sent to the
-- server), whether this install accepted the invite (only that install launches the game on
-- `invite.join`), whether `invite.join` was queued, and the progress-report throttle.
CREATE TABLE social_invites_local (
    server_id         TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    invite_id         TEXT NOT NULL,
    package_id        TEXT NOT NULL,
    role              TEXT NOT NULL CHECK (role IN ('sent', 'accepted')),
    secret_nonce      BLOB CHECK (secret_nonce IS NULL OR length(secret_nonce) = 24),
    secret            BLOB,
    join_sent         INTEGER NOT NULL DEFAULT 0 CHECK (join_sent IN (0, 1)),
    last_report_at    INTEGER,
    last_progress     REAL,
    created_at        INTEGER NOT NULL,
    PRIMARY KEY (server_id, invite_id),
    CHECK ((secret IS NULL) = (secret_nonce IS NULL)),
    CHECK (role = 'sent' OR secret IS NULL)
);
