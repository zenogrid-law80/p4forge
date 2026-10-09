ALTER TABLE runs ADD COLUMN cancel_requested boolean NOT NULL DEFAULT false;
CREATE TABLE agents (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    project_id uuid NOT NULL REFERENCES projects(id),
    pool text NOT NULL,
    os text NOT NULL CHECK (os IN ('linux')),
    arch text NOT NULL CHECK (arch IN ('x86_64','aarch64')),
    token_hash bytea NOT NULL UNIQUE,
    active boolean NOT NULL DEFAULT true,
    draining boolean NOT NULL DEFAULT false,
    last_seen_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE job_attempts (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL UNIQUE REFERENCES job_runs(id),
    agent_id uuid NOT NULL REFERENCES agents(id),
    claim_request_id uuid NOT NULL,
    fence bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    status text NOT NULL CHECK (status IN ('running','succeeded','failed','canceled','lost')),
    lease_expires_at timestamptz NOT NULL,
    cancel_requested boolean NOT NULL DEFAULT false,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    exit_code integer,
    reason text NOT NULL DEFAULT '',
    completion_hash text,
    last_log_sequence bigint NOT NULL DEFAULT -1,
    log_bytes bigint NOT NULL DEFAULT 0 CHECK (log_bytes BETWEEN 0 AND 4194304),
    UNIQUE(agent_id,claim_request_id)
);
CREATE UNIQUE INDEX one_active_job_per_agent ON job_attempts(agent_id) WHERE status='running';
CREATE INDEX attempt_leases ON job_attempts(lease_expires_at) WHERE status='running';
CREATE TABLE attempt_logs (
    attempt_id uuid NOT NULL REFERENCES job_attempts(id),
    sequence bigint NOT NULL CHECK (sequence >= 0),
    stream text NOT NULL CHECK (stream IN ('stdout','stderr','system')),
    content text NOT NULL CHECK (octet_length(content) <= 16384),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(attempt_id,sequence)
);
