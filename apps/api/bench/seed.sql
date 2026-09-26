-- Synthetic data for the query-plan review and load tests (A1-T16).
--
--   createdb vgames_bench && sqlx migrate run --source apps/api/migrations --database-url …/vgames_bench
--   psql -v ON_ERROR_STOP=1 -d vgames_bench -f apps/api/bench/seed.sql
--
-- 20k users (5 admins) with a desktop session each, 100k packages (95k published with a windows
-- release, ~5.3k of them also linux, 5k drafts), one yanked older version per 10 packages,
-- 16 packs per published version, 1M audit rows, 1M message envelopes across 2k devices,
-- 200k finished jobs.
-- The access token of user n (discord id 100000000000 + n) is `vga_` + `bench<discord id>` padded
-- with `A` to 43 characters; load.sh derives its tokens from the same rule.
-- Fake digests and signatures only: nothing here verifies.

\timing on
SET synchronous_commit = off;

BEGIN;

INSERT INTO users (discord_id, username, role, created_at)
SELECT (100000000000 + g)::text, 'user' || g, CASE WHEN g <= 5 THEN 'admin' ELSE 'user' END,
       now() - (g || ' seconds')::interval
FROM generate_series(1, 20000) g;

INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash, refresh_expires_at)
SELECT u.id, 'desktop', sha256(('vga_' || rpad('bench' || u.discord_id, 43, 'A'))::bytea), now() + interval '1 day',
       sha256(('vgr_' || rpad('bench' || u.discord_id, 43, 'A'))::bytea), now() + interval '30 days'
FROM users u;

INSERT INTO trust_bundles (version, bundle, signature) VALUES (1, '\x7b7d', decode(repeat('00', 64), 'hex'));
INSERT INTO publisher_keys (key_id, public_key, holder_user_id, label, not_before, not_after, trust_version)
SELECT repeat('ab', 16), decode(repeat('01', 32), 'hex'), id, 'bench', now() - interval '1 day', now() + interval '1 year', 1
FROM users WHERE role = 'admin' ORDER BY id LIMIT 1;

-- Titles mix words so trigram search has realistic selectivity. Genre and draft status come from
-- independent hashes, so no filter lines up with title order.
INSERT INTO packages (slug, title, summary, genres, status, created_by, updated_at)
SELECT 'bench-' || g,
       (ARRAY['Portal', 'Hollow', 'Night', 'Star', 'Iron', 'Crystal', 'Shadow', 'Pixel', 'Dungeon', 'Ocean'])[1 + g % 10]
         || ' ' || (ARRAY['Knight', 'Racer', 'Quest', 'Legends', 'Tactics', 'Runner', 'Garden', 'Frontier'])[1 + (g / 10) % 8]
         || ' ' || g,
       'Synthetic package ' || g,
       ARRAY[(ARRAY['action', 'puzzle', 'rpg', 'strategy', 'indie'])[1 + abs(hashtext('genre' || g)) % 5]],
       CASE WHEN abs(hashtext('draft' || g)) % 20 = 0 THEN 'draft' ELSE 'published' END,
       (SELECT id FROM users WHERE role = 'admin' ORDER BY id LIMIT 1),
       now() - ((g * 7) || ' seconds')::interval
FROM generate_series(1, 100000) g;

-- One published version per published package (windows), a linux one for every 18th, spread
-- evenly over titles.
INSERT INTO package_versions (package_id, platform, sequence, version_label, state, pack_count, total_size,
                              file_count, chunk_count, manifest_object, manifest_size, manifest_blake3, signature,
                              publisher_key_id, created_by, finalized_at, verified_at, published_at)
SELECT p.id, pl.platform, 2, '1.1', 'published', 16, 16 * 268435456::bigint, 1000, 4096,
       'v1/' || p.id || '/' || pl.platform || '/manifest.json', 65536,
       sha256(p.id::text::bytea), decode(repeat('00', 64), 'hex'), repeat('ab', 16), p.created_by,
       now(), now(), now()
FROM packages p
CROSS JOIN LATERAL (VALUES ('windows-x86_64'), ('linux-x86_64')) pl(platform)
WHERE p.status = 'published'
  AND (pl.platform = 'windows-x86_64' OR abs(hashtext(p.id::text)) % 18 = 0);

-- A yanked predecessor for every 10th published package.
INSERT INTO package_versions (package_id, platform, sequence, version_label, state, yank_reason, pack_count, total_size,
                              file_count, chunk_count, manifest_object, manifest_size, manifest_blake3, signature,
                              publisher_key_id, created_by, finalized_at, verified_at, published_at, yanked_at)
SELECT v.package_id, v.platform, 1, '1.0', 'yanked', 'broken save files', 16, v.total_size, 1000, 4096,
       v.manifest_object || '.old', 65536, v.manifest_blake3, v.signature, v.publisher_key_id, v.created_by,
       now(), now(), now(), now()
FROM package_versions v
WHERE v.platform = 'windows-x86_64' AND hashtext(v.package_id::text) % 10 = 0;

INSERT INTO package_releases (package_id, platform, version_id, updated_by)
SELECT package_id, platform, id, created_by FROM package_versions WHERE state = 'published';

INSERT INTO package_packs (version_id, pack_index, object_name, size, blake3, uploaded_at)
SELECT v.id, i, 'v1/' || v.package_id || '/' || v.id || '/packs/' || lpad(i::text, 5, '0') || '.pack',
       268435456, sha256((v.id::text || i)::bytea), now()
FROM package_versions v CROSS JOIN generate_series(0, 15) i
WHERE v.state = 'published';

-- 1M audit rows over 90 days, 12 actions, 5 admins. Ids are uuidv7 taken at insert time, so
-- created_at must grow with g as it does in production (newest id = newest row).
INSERT INTO audit_log (actor_user_id, action, target_type, target_id, details, created_at)
SELECT (SELECT array_agg(id ORDER BY id) FROM users WHERE role = 'admin')[1 + g % 5],
       (ARRAY['package.create', 'package.update', 'version.create', 'version.finalize', 'version.publish',
              'version.yank', 'asset.upload', 'user.disable', 'user.role_change', 'allowlist.add',
              'settings.update', 'job.retry'])[1 + g % 12],
       'package', 'pkg-' || (g % 50000), '{}', now() - (((1000000 - g) * 7.776) || ' seconds')::interval
FROM generate_series(1, 1000000) g;

-- 2k devices (first 2k users), 20k direct conversations, 1M envelopes (~500 per device).
INSERT INTO devices (user_id, display_name, platform)
SELECT id, 'bench device', 'linux' FROM users ORDER BY id LIMIT 2000;

INSERT INTO conversations (kind, direct_key, created_by)
SELECT 'direct', 'bench:' || g, (SELECT id FROM users ORDER BY id LIMIT 1) FROM generate_series(1, 20000) g;

INSERT INTO message_envelopes (conversation_id, sender_user_id, sender_device_id, recipient_device_id,
                               client_message_id, algorithm, olm_message_type, ciphertext)
SELECT c.ids[1 + g % 20000], d.users[1 + g % 2000], d.ids[1 + g % 2000], d.ids[1 + (g * 7 + 1) % 2000],
       uuidv7(), 'olm.v1', 1, '\x00'
FROM generate_series(1, 1000000) g,
     (SELECT array_agg(id ORDER BY id) AS ids FROM conversations) c,
     (SELECT array_agg(id ORDER BY id) AS ids, array_agg(user_id ORDER BY id) AS users FROM devices) d;

-- 200k finished jobs (1% failed, 0.1% dead) and 50 queued, half of them due.
INSERT INTO jobs (kind, state, attempts, run_at, created_at, updated_at, finished_at)
SELECT (ARRAY['metadata.fetch', 'version.verify', 'saves.gc'])[1 + g % 3],
       CASE WHEN g % 1000 = 0 THEN 'dead' WHEN g % 100 = 0 THEN 'failed' ELSE 'succeeded' END,
       1, now() - (g || ' minutes')::interval, now() - (g || ' minutes')::interval, now(), now()
FROM generate_series(1, 200000) g;
INSERT INTO jobs (kind, state, run_at)
SELECT 'metadata.fetch', 'queued', now() + (g - 25) * interval '1 minute' FROM generate_series(1, 50) g;

COMMIT;

VACUUM ANALYZE;
