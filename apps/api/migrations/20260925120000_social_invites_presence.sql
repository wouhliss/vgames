-- Social additions (docs/architecture/05-social-notes.md §2.4, §2.5, §3). Owner: Agent 4.
-- Append-only: never edit after merge.

-- Presence liveness: every 10 s each API instance refreshes heartbeat_at for the users with a
-- socket on it; a row whose heartbeat is older than 30 s becomes offline.
ALTER TABLE user_presence
  ADD COLUMN heartbeat_at timestamptz NOT NULL DEFAULT now();
CREATE INDEX user_presence_stale_idx ON user_presence (heartbeat_at) WHERE status <> 'offline';

-- Invites: why an invite failed (reported by the invitee), and when progress was last
-- published (at most one progress event per invite every 2 s).
ALTER TABLE game_invites
  ADD COLUMN failure_reason text
    CHECK (failure_reason IN ('no_build_for_platform', 'install_failed', 'insufficient_space', 'cancelled_by_user')),
  ADD COLUMN progress_published_at timestamptz,
  ADD CONSTRAINT game_invites_failure_reason_state CHECK (failure_reason IS NULL OR state = 'failed');

-- Pending outgoing requests per user (limit 100).
CREATE INDEX friendships_requested_by_idx ON friendships (requested_by) WHERE state = 'pending';

-- social.sweep deletes friend codes a day after they expire.
CREATE INDEX friend_codes_expiry_idx ON friend_codes (expires_at);
