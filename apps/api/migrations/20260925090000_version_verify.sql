-- Verification progress (A1-T12) and the reason a version was withdrawn.
ALTER TABLE package_versions
  ADD COLUMN verify_progress real CHECK (verify_progress BETWEEN 0 AND 1),
  ADD COLUMN yank_reason text CHECK (char_length(yank_reason) BETWEEN 3 AND 500),
  ADD CONSTRAINT package_versions_yanked_has_reason CHECK (state <> 'yanked' OR yank_reason IS NOT NULL);
