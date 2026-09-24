-- A1-T04: fields the Discord sign-in flow needs.

-- The launcher's own state value, echoed back in vgames://auth/callback so it can match
-- the deep link to the sign-in it started.
ALTER TABLE oauth_flows
  ADD COLUMN client_state text CHECK (client_state ~ '^[A-Za-z0-9_-]{16,64}$');

-- Shown in "your sessions" lists (launcher device name, or empty for web).
ALTER TABLE sessions
  ADD COLUMN device_name text CHECK (char_length(device_name) <= 64);
