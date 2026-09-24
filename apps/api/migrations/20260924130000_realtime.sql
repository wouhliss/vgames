-- A1-T05: realtime gateway.

-- Single-use WebSocket tickets (30 s). Stored as SHA-256 digests; any API instance can redeem.
CREATE UNLOGGED TABLE realtime_tickets (
  digest      bytea PRIMARY KEY CHECK (octet_length(digest) = 32),
  session_id  uuid NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
  expires_at  timestamptz NOT NULL
);
CREATE INDEX realtime_tickets_expiry_idx ON realtime_tickets (expires_at);

-- Events too large for a NOTIFY payload (> 7.5 KB) are sent by reference. Short-lived.
CREATE UNLOGGED TABLE realtime_events (
  id          uuid PRIMARY KEY DEFAULT uuidv7(),
  payload     jsonb NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX realtime_events_created_idx ON realtime_events (created_at);
