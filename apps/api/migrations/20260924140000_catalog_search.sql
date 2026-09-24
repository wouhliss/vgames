-- A1-T09: fast case-insensitive title search for the catalog (ILIKE '%q%').
-- pg_trgm is a trusted extension: the database owner can create it.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX packages_title_trgm_idx ON packages USING gin (lower(title) gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX packages_updated_idx ON packages (updated_at DESC, id DESC) WHERE deleted_at IS NULL;
