-- Perforce can renumber a pending changelist during submit. Preserve both IDs.
ALTER TABLE run_publications ADD COLUMN reserved_changelist bigint;
UPDATE run_publications SET reserved_changelist=changelist;
ALTER TABLE run_publications ALTER COLUMN reserved_changelist SET NOT NULL;
ALTER TABLE run_publications ADD CHECK (reserved_changelist > 0);
