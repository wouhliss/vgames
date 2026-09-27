-- A4-T06: game invites (05-social §5, 04-database §3). Owner: Agent 4.
-- Append-only: never edit after merge. One active invite per (sender, invitee, package) is
-- already enforced by game_invites_one_active_uq (init migration).

-- The sender's invites (list, cancel).
CREATE INDEX game_invites_from_idx ON game_invites (from_user_id, state);
