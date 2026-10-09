CREATE TABLE artifact_manifests (
    attempt_id uuid PRIMARY KEY REFERENCES job_attempts(id),
    run_id uuid NOT NULL REFERENCES runs(id),
    definition_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE artifacts (
    id uuid PRIMARY KEY,
    attempt_id uuid NOT NULL REFERENCES artifact_manifests(attempt_id),
    run_id uuid NOT NULL REFERENCES runs(id),
    name text NOT NULL,
    path text NOT NULL,
    size bigint NOT NULL CHECK (size BETWEEN 0 AND 67108864),
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    uploaded_size bigint NOT NULL DEFAULT 0 CHECK (uploaded_size >= 0 AND uploaded_size <= size),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','verified')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(attempt_id,name)
);
CREATE INDEX artifacts_run ON artifacts(run_id);
