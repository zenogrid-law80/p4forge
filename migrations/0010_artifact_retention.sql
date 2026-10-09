CREATE TABLE artifact_retention_policies (
    project_id uuid PRIMARY KEY REFERENCES projects(id),
    generation bigint NOT NULL DEFAULT 0,
    enabled boolean NOT NULL DEFAULT false,
    retention_days integer NOT NULL DEFAULT 30 CHECK (retention_days BETWEEN 1 AND 3650),
    keep_successful_runs integer NOT NULL DEFAULT 5 CHECK (keep_successful_runs BETWEEN 1 AND 1000),
    updated_at timestamptz,
    updated_by uuid REFERENCES users(id),
    last_sweep_at timestamptz
);
ALTER TABLE artifacts ADD COLUMN purge_requested_at timestamptz;
ALTER TABLE artifacts ADD COLUMN purge_reason text CHECK (purge_reason IN ('expired','incomplete'));
ALTER TABLE artifacts ADD COLUMN purge_generation bigint;
ALTER TABLE artifacts ADD COLUMN purge_last_attempt_at timestamptz;
ALTER TABLE artifacts ADD COLUMN purge_error text;
ALTER TABLE artifacts ADD COLUMN deleted_at timestamptz;
ALTER TABLE artifacts ADD CONSTRAINT artifact_deletion_has_intent CHECK (
    (purge_requested_at IS NULL AND purge_reason IS NULL AND purge_generation IS NULL AND deleted_at IS NULL)
    OR (purge_requested_at IS NOT NULL AND purge_reason IS NOT NULL AND purge_generation IS NOT NULL)
);
CREATE INDEX artifacts_pending_purge ON artifacts(purge_last_attempt_at NULLS FIRST, id)
    WHERE purge_requested_at IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX runs_retention_history ON runs(pipeline_id, finished_at DESC, id DESC)
    WHERE status='succeeded';
