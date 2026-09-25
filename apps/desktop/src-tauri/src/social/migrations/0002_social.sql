-- Social and E2EE local state (A4-T02, docs/architecture/05-social.md §4.3). Owner: Agent 4.
-- Every server is its own identity island: all rows are scoped by server_id and go away with
-- the server (ON DELETE CASCADE). Times are Unix seconds; ids are lowercase hyphenated UUIDs.
-- Olm pickles are encrypted with the keychain pickle key; message bodies and queued payloads
-- with XChaCha20-Poly1305 under the keychain chat key. Append-only: never edit after release.

-- This install's Olm account on a server.
CREATE TABLE social_accounts (
    server_id       TEXT PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    user_id         TEXT NOT NULL,
    device_id       TEXT,                            -- set once POST /v1/devices succeeded
    account_pickle  TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

-- Pairwise Olm sessions. Several may exist per peer device (both sides can start one).
CREATE TABLE social_olm_sessions (
    server_id          TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    session_id         TEXT NOT NULL,
    peer_device_id     TEXT NOT NULL,
    peer_identity_key  TEXT NOT NULL,
    pickle             TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    last_used_at       INTEGER NOT NULL,
    PRIMARY KEY (server_id, session_id)
);
CREATE INDEX social_olm_sessions_peer ON social_olm_sessions (server_id, peer_device_id, last_used_at);

-- Contact devices (and this user's other devices), pinned on first sight (TOFU per device).
-- A changed key for a known device id is kept aside (pending_*) and blocks sending until the
-- user trusts it; revoked devices stay listed so they are never silently re-added.
CREATE TABLE social_devices (
    server_id             TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    device_id             TEXT NOT NULL,
    user_id               TEXT NOT NULL,
    display_name          TEXT,
    identity_key          TEXT NOT NULL,
    signing_key           TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK (state IN ('trusted', 'key_changed', 'revoked')),
    pending_identity_key  TEXT,
    pending_signing_key   TEXT,
    first_seen_at         INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    PRIMARY KEY (server_id, device_id),
    CHECK ((state = 'key_changed') = (pending_identity_key IS NOT NULL AND pending_signing_key IS NOT NULL))
);
CREATE INDEX social_devices_user ON social_devices (server_id, user_id);

-- Per-contact verification: the safety number the user confirmed. A different current number
-- means a device changed since, and the contact needs re-verification.
CREATE TABLE social_contacts (
    server_id               TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    user_id                 TEXT NOT NULL,
    verified_safety_number  TEXT CHECK (verified_safety_number IS NULL OR length(verified_safety_number) = 60),
    updated_at              INTEGER NOT NULL,
    PRIMARY KEY (server_id, user_id)
);

-- Conversation list cache (the server is the source of truth for membership).
CREATE TABLE social_conversations (
    server_id         TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    id                TEXT NOT NULL,
    kind              TEXT NOT NULL CHECK (kind IN ('direct', 'party')),
    members           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(members)),
    created_at        INTEGER NOT NULL,
    last_activity_at  INTEGER NOT NULL,
    last_read_seq     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (server_id, id)
);

-- Decrypted history, bodies encrypted at rest. (sender_device_id, client_message_id) is unique,
-- so a message delivered twice is stored once.
CREATE TABLE social_messages (
    seq                INTEGER PRIMARY KEY AUTOINCREMENT,
    server_id          TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    id                 TEXT NOT NULL,
    conversation_id    TEXT NOT NULL,
    sender_user_id     TEXT NOT NULL,
    sender_device_id   TEXT NOT NULL,
    client_message_id  TEXT NOT NULL,
    direction          TEXT NOT NULL CHECK (direction IN ('in', 'out')),
    status             TEXT NOT NULL CHECK (status IN ('pending', 'sent', 'failed', 'received')),
    body_nonce         BLOB NOT NULL CHECK (length(body_nonce) = 24),
    body               BLOB NOT NULL,
    sent_at            INTEGER NOT NULL,
    received_at        INTEGER,
    UNIQUE (server_id, id),
    UNIQUE (server_id, sender_device_id, client_message_id)
);
CREATE INDEX social_messages_conversation ON social_messages (server_id, conversation_id, seq);

-- Messages waiting to be sent (offline, rate limited, server errors). Plaintext payloads are
-- encrypted with the chat key until they are Olm-encrypted per device at send time.
CREATE TABLE social_outbox (
    server_id          TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    client_message_id  TEXT NOT NULL,
    message_id         TEXT NOT NULL,
    conversation_id    TEXT NOT NULL,
    payload_nonce      BLOB NOT NULL CHECK (length(payload_nonce) = 24),
    payload            BLOB NOT NULL,
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at    INTEGER NOT NULL,
    last_error         TEXT,
    created_at         INTEGER NOT NULL,
    PRIMARY KEY (server_id, client_message_id)
);
CREATE INDEX social_outbox_due ON social_outbox (server_id, next_attempt_at);

-- Envelope ids already decrypted and stored, so a redelivery (lost ack) is acknowledged
-- without touching the ratchet. Pruned after the server's 30-day envelope retention.
CREATE TABLE social_processed_envelopes (
    server_id     TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    envelope_id   TEXT NOT NULL,
    processed_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, envelope_id)
);

-- Users this account blocked (the server has no list endpoint).
CREATE TABLE social_blocks (
    server_id   TEXT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL,
    username    TEXT,
    blocked_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, user_id)
);
