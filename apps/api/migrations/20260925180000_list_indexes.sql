-- Indexes from the query-plan review on seeded data (A1-T16; apps/api/bench/).
--
-- The admin audit log pages by id (uuidv7, newest first) under any mix of filters. Each filter
-- gets a (key, id DESC) index so a rare actor, action or target reads only its own rows instead
-- of walking the whole log backwards (60-260 ms on 1M rows before, under 1 ms after).
DROP INDEX audit_log_actor_idx;
CREATE INDEX audit_log_actor_idx ON audit_log (actor_user_id, id DESC);
DROP INDEX audit_log_target_idx;
CREATE INDEX audit_log_target_idx ON audit_log (target_type, target_id, id DESC);
CREATE INDEX audit_log_action_idx ON audit_log (action, id DESC);

-- Admin job list filtered by state: dead and failed jobs are rare among finished ones.
CREATE INDEX jobs_state_idx ON jobs (state, id DESC);
