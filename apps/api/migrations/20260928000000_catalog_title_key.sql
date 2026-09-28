-- Keep the lowercase title in the table so ordered catalog scans can read it
-- directly from covering indexes without visiting every matching package row.
ALTER TABLE packages ADD COLUMN title_key text GENERATED ALWAYS AS (lower(title)) STORED NOT NULL;

CREATE INDEX packages_catalog_title_key_idx ON packages (title_key, id)
  INCLUDE (updated_at) WHERE deleted_at IS NULL AND status = 'published';
CREATE INDEX packages_catalog_recent_cover_idx ON packages (updated_at DESC, id DESC)
  INCLUDE (title_key) WHERE deleted_at IS NULL AND status = 'published';
