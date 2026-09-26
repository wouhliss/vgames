-- EXPLAIN (ANALYZE, BUFFERS) of the API's hot queries on the seeded database (A1-T16).
--
--   psql -d vgames_bench -f apps/api/bench/explain.sql > explain.txt
--
-- Statements are the handlers' SQL verbatim, PREPAREd as sqlx sends them, with the plan cache
-- mode `db::connect` sets on every pool connection (force_custom_plan: the optional filters
-- `$n IS NULL OR …` fold away per call). Pass `-v generic=1` to see the generic plans instead.

\pset pager off
\if :{?generic}
SET plan_cache_mode = force_generic_plan;
\else
SET plan_cache_mode = force_custom_plan;
\endif

-- Sample ids (EXECUTE cannot take subqueries).
SELECT r.package_id AS pkg, r.version_id AS ver FROM package_releases r ORDER BY r.package_id OFFSET 777 LIMIT 1 \gset
SELECT array_agg(id)::text AS page_ids
FROM (SELECT id FROM packages WHERE status = 'published' ORDER BY lower(title) LIMIT 50) x \gset
SELECT id AS dev FROM devices ORDER BY id OFFSET 500 LIMIT 1 \gset
SELECT id AS admin_id FROM users WHERE role = 'admin' ORDER BY id OFFSET 2 LIMIT 1 \gset

\echo '== 1. session lookup (every authenticated request)'
PREPARE session_lookup(bytea) AS
SELECT s.id AS session_id, s.kind, s.device_id, s.access_expires_at, s.csrf_token_hash,
       s.created_at, s.last_used_at, u.id AS user_id, u.role, u.disabled_at
FROM sessions s JOIN users u ON u.id = s.user_id
WHERE s.access_token_hash = $1 AND s.revoked_at IS NULL;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE session_lookup(sha256(('vga_' || rpad('bench100000012345', 43, 'A'))::bytea));

\echo '== 2. catalog page (GET /v1/packages)'
-- (genre, cursor key, platform, recent, cursor id, limit + 1)
PREPARE catalog(text, text, text, boolean, uuid, bigint) AS
SELECT p.id, lower(p.title) AS title_key, p.updated_at
FROM packages p
WHERE p.deleted_at IS NULL AND p.status = 'published'
  AND EXISTS (SELECT 1 FROM package_releases r WHERE r.package_id = p.id
              AND ($3::text IS NULL OR r.platform = $3))
  AND ($1::text IS NULL OR p.genres @> ARRAY[$1::text])
  AND ($2::text IS NULL OR
       CASE WHEN $4 THEN (p.updated_at, p.id) < ($2::timestamptz, $5)
            ELSE (lower(p.title), p.id) > ($2, $5) END)
ORDER BY
  CASE WHEN NOT $4 THEN lower(p.title) END,
  CASE WHEN NOT $4 THEN p.id END,
  CASE WHEN $4 THEN p.updated_at END DESC,
  CASE WHEN $4 THEN p.id END DESC
LIMIT $6;
-- The same with a title search ($7): matches are sorted before the release check.
PREPARE catalog_search(text, text, text, boolean, uuid, bigint, text) AS
SELECT m.id, m.title_key, m.updated_at
FROM (
  SELECT p.id, lower(p.title) AS title_key, p.updated_at
  FROM packages p
  WHERE p.deleted_at IS NULL AND p.status = 'published'
    AND lower(p.title) LIKE $7 ESCAPE '\'
    AND ($1::text IS NULL OR p.genres @> ARRAY[$1::text])
    AND ($2::text IS NULL OR
         CASE WHEN $4 THEN (p.updated_at, p.id) < ($2::timestamptz, $5)
              ELSE (lower(p.title), p.id) > ($2, $5) END)
  ORDER BY
    CASE WHEN NOT $4 THEN lower(p.title) END,
    CASE WHEN NOT $4 THEN p.id END,
    CASE WHEN $4 THEN p.updated_at END DESC,
    CASE WHEN $4 THEN p.id END DESC
  OFFSET 0
) m
WHERE EXISTS (SELECT 1 FROM package_releases r WHERE r.package_id = m.id
              AND ($3::text IS NULL OR r.platform = $3))
ORDER BY
  CASE WHEN NOT $4 THEN m.title_key END,
  CASE WHEN NOT $4 THEN m.id END,
  CASE WHEN $4 THEN m.updated_at END DESC,
  CASE WHEN $4 THEN m.id END DESC
LIMIT $6;
\echo '-- 2a. first page, by title'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog(NULL, NULL, NULL, false, '00000000-0000-0000-0000-000000000000', 51);
\echo '-- 2b. first page, most recent'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog(NULL, NULL, NULL, true, '00000000-0000-0000-0000-000000000000', 51);
\echo '-- 2c. deep page by title (cursor)'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog(NULL, 'shadow knight', NULL, false, '00000000-0000-0000-0000-000000000000', 51);
\echo '-- 2c2. deep page, most recent (cursor)'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog(NULL, (now() - interval '5 days')::text, NULL, true, 'ffffffff-ffff-ffff-ffff-ffffffffffff', 51);
\echo '-- 2d. search "%hollow garden%"'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog_search(NULL, NULL, NULL, false, '00000000-0000-0000-0000-000000000000', 51, '%hollow garden%');
\echo '-- 2d2. broad search "%knight%" (~12k matches)'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog_search(NULL, NULL, NULL, false, '00000000-0000-0000-0000-000000000000', 51, '%knight%');
\echo '-- 2d3. broad search, most recent'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog_search(NULL, NULL, NULL, true, '00000000-0000-0000-0000-000000000000', 51, '%knight%');
\echo '-- 2d4. rare search "%garden 4242%"'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog_search(NULL, NULL, NULL, false, '00000000-0000-0000-0000-000000000000', 51, '%garden 4242%');
\echo '-- 2e. linux platform'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog(NULL, NULL, 'linux-x86_64', false, '00000000-0000-0000-0000-000000000000', 51);
\echo '-- 2f. genre + linux platform'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog('puzzle', NULL, 'linux-x86_64', false, '00000000-0000-0000-0000-000000000000', 51);
\echo '-- 2g. genre only, most recent'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE catalog('rpg', NULL, NULL, true, '00000000-0000-0000-0000-000000000000', 51);

\echo '== 3. catalog page details for 50 ids'
PREPARE page_releases(uuid[]) AS
SELECT r.package_id, r.platform, v.id AS version_id, v.version_label, v.sequence
FROM package_releases r JOIN package_versions v ON v.id = r.version_id
WHERE r.package_id = ANY($1) AND v.total_size IS NOT NULL AND v.published_at IS NOT NULL
ORDER BY r.platform;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
EXECUTE page_releases(:'page_ids');

\echo '== 4. release descriptor (GET /v1/packages/{id}/releases/{platform})'
PREPARE release(uuid, text) AS
SELECT v.id, v.sequence, v.version_label, v.total_size, v.pack_count, v.manifest_object, v.manifest_size,
       v.manifest_blake3, v.signature, v.publisher_key_id, v.published_at, p.status
FROM package_releases r
JOIN package_versions v ON v.id = r.version_id
JOIN packages p ON p.id = r.package_id AND p.deleted_at IS NULL
WHERE r.package_id = $1 AND r.platform = $2 AND v.state = 'published';
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
EXECUTE release(:'pkg', 'windows-x86_64');
PREPARE yanked(uuid, text) AS
SELECT id FROM package_versions WHERE package_id = $1 AND platform = $2 AND state = 'yanked' ORDER BY sequence;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
EXECUTE yanked(:'pkg', 'windows-x86_64');

\echo '== 5. download URLs (POST /v1/versions/{id}/download-urls)'
PREPARE packs(uuid, int[]) AS
SELECT pack_index, object_name, size FROM package_packs
WHERE version_id = $1 AND pack_index = ANY($2) ORDER BY pack_index;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
EXECUTE packs(:'ver', '{0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15}');

\echo '== 6. inbox (Agent 4, A4-T05: GET /v1/inbox for one device)'
PREPARE inbox(uuid, uuid, bigint) AS
SELECT id, conversation_id, sender_user_id, sender_device_id, olm_message_type, ciphertext, created_at
FROM message_envelopes
WHERE recipient_device_id = $1 AND ($2::uuid IS NULL OR id > $2) AND expires_at > now()
ORDER BY id LIMIT $3;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
EXECUTE inbox(:'dev', NULL, 101);

\echo '== 7. audit log (GET /v1/admin/audit-log)'
PREPARE audit(uuid, uuid, text, text, text, timestamptz, timestamptz, bigint) AS
SELECT a.id, a.action, a.target_type, a.target_id, host(a.ip) AS ip, a.details, a.created_at,
       u.id AS actor_id, u.username
FROM audit_log a LEFT JOIN users u ON u.id = a.actor_user_id
WHERE ($1::uuid IS NULL OR a.id < $1)
  AND ($2::uuid IS NULL OR a.actor_user_id = $2)
  AND ($3::text IS NULL OR a.action = $3)
  AND ($4::text IS NULL OR a.target_type = $4)
  AND ($5::text IS NULL OR a.target_id = $5)
  AND ($6::timestamptz IS NULL OR a.created_at >= $6)
  AND ($7::timestamptz IS NULL OR a.created_at <= $7)
ORDER BY a.id DESC LIMIT $8;
\echo '-- 7a. newest page'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE audit(NULL, NULL, NULL, NULL, NULL, NULL, NULL, 51);
\echo '-- 7b. one action'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE audit(NULL, NULL, 'job.retry', NULL, NULL, NULL, NULL, 51);
\echo '-- 7b2. an action that never happened (worst case without an index)'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE audit(NULL, NULL, 'trust.bundle_upload', NULL, NULL, NULL, NULL, 51);
\echo '-- 7c. one target'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE audit(NULL, NULL, NULL, 'package', 'pkg-4242', NULL, NULL, 51);
\echo '-- 7d. one actor, last week'
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
EXECUTE audit(NULL, :'admin_id', NULL, NULL, NULL, now() - interval '7 days', NULL, 51);

\echo '== 8. job claim (every worker poll)'
PREPARE claim(text[]) AS
SELECT id FROM jobs
WHERE state = 'queued' AND run_at <= now() AND kind = ANY($1)
ORDER BY priority DESC, run_at LIMIT 1 FOR UPDATE SKIP LOCKED;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE claim('{metadata.fetch,version.verify,saves.gc}');

\echo '== 9. admin job list (GET /v1/admin/jobs?state=dead)'
PREPARE jobs_list(uuid, text, text, bigint) AS
SELECT id, kind, state, attempts, max_attempts, last_error, run_at, created_at, finished_at
FROM jobs
WHERE ($1::uuid IS NULL OR id < $1)
  AND ($2::text IS NULL OR state = $2)
  AND ($3::text IS NULL OR kind = $3)
ORDER BY id DESC LIMIT $4;
EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) EXECUTE jobs_list(NULL, 'dead', NULL, 51);
