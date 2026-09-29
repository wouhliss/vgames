-- A2-T07: account display names, and the persistent trust block.

ALTER TABLE accounts ADD COLUMN display_name TEXT;

-- Fingerprint the server presented when it did not match the pinned root key.
-- While set, every request to the server is refused (no bypass); it clears only
-- when the server presents the pinned key again (01-security §3.1).
ALTER TABLE servers ADD COLUMN blocked_fingerprint TEXT;
