-- A4-T05: conversations and the ciphertext relay (05-social §4.1, §4.4). Owner: Agent 4.
-- Append-only: never edit after merge.

-- Envelopes are deleted once acknowledged, so the conversation remembers its last send
-- (conversation list order and `last_activity_at`).
ALTER TABLE conversations ADD COLUMN last_message_at timestamptz;

-- Members of a conversation (fan-out checks, unknown_devices).
CREATE INDEX conversation_members_conversation_idx ON conversation_members (conversation_id) WHERE left_at IS NULL;
