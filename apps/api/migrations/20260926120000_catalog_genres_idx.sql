-- Catalog genre filter (A1-T16): `genres @> ARRAY[$genre]` can use this index, so a genre
-- carried by few packages no longer scans the whole catalog (25 ms -> 0.5 ms on 100k packages).
-- Common genres keep walking the title or recency index.
CREATE INDEX packages_genres_idx ON packages USING gin (genres) WHERE deleted_at IS NULL;
