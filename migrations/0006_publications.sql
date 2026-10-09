CREATE TABLE run_publications (
    run_id uuid PRIMARY KEY REFERENCES runs(id),
    attempt_id uuid NOT NULL UNIQUE REFERENCES job_attempts(id),
    changelist bigint NOT NULL CHECK (changelist > 0),
    status text NOT NULL CHECK (status IN ('prepared','submitted','unchanged')),
    created_at timestamptz NOT NULL DEFAULT now(),
    verified_at timestamptz
);
CREATE TABLE run_followups (
    id uuid PRIMARY KEY,
    parent_run_id uuid NOT NULL REFERENCES runs(id),
    pipeline_id uuid NOT NULL,
    version bigint NOT NULL,
    child_run_id uuid UNIQUE REFERENCES runs(id),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','created','blocked')),
    reason text NOT NULL DEFAULT '',
    FOREIGN KEY(pipeline_id,version) REFERENCES pipeline_revisions(pipeline_id,version),
    UNIQUE(parent_run_id,pipeline_id)
);
