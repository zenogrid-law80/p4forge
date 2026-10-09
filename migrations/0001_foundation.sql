CREATE TABLE users (
    id uuid PRIMARY KEY,
    google_sub text NOT NULL UNIQUE,
    email text NOT NULL,
    name text NOT NULL,
    is_admin boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE oauth_states (
    state_hash bytea PRIMARY KEY,
    nonce text NOT NULL,
    verifier text NOT NULL,
    expires_at timestamptz NOT NULL
);
CREATE TABLE auth_sessions (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_hash bytea NOT NULL,
    expires_at timestamptz NOT NULL
);
CREATE INDEX auth_sessions_expiry ON auth_sessions(expires_at);
CREATE TABLE projects (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE project_members (
    project_id uuid NOT NULL REFERENCES projects(id),
    user_id uuid NOT NULL REFERENCES users(id),
    role text NOT NULL CHECK (role IN ('viewer','operator','editor','admin')),
    PRIMARY KEY(project_id,user_id)
);
CREATE TABLE perforce_connections (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    definition jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE perforce_roots (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id),
    connection_id uuid NOT NULL REFERENCES perforce_connections(id),
    name text NOT NULL,
    definition jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(project_id,name)
);
CREATE TABLE pipelines (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id),
    name text NOT NULL,
    current_version bigint NOT NULL DEFAULT 0 CHECK (current_version >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(project_id,name)
);
CREATE TABLE pipeline_drafts (
    pipeline_id uuid PRIMARY KEY REFERENCES pipelines(id),
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    base_version bigint NOT NULL DEFAULT 0,
    definition jsonb,
    edited_by uuid REFERENCES users(id),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE pipeline_revisions (
    pipeline_id uuid NOT NULL REFERENCES pipelines(id),
    version bigint NOT NULL CHECK (version > 0),
    definition jsonb NOT NULL,
    definition_hash text NOT NULL,
    published_by uuid NOT NULL REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(pipeline_id,version)
);
CREATE FUNCTION protect_pipeline_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'published pipeline revisions are immutable';
END;
$$;
CREATE TRIGGER immutable_pipeline_revision BEFORE UPDATE OR DELETE ON pipeline_revisions
FOR EACH ROW EXECUTE FUNCTION protect_pipeline_revision();
CREATE FUNCTION validate_current_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.current_version > 0 AND NOT EXISTS (
        SELECT 1 FROM pipeline_revisions WHERE pipeline_id=NEW.id AND version=NEW.current_version
    ) THEN RAISE EXCEPTION 'current_version must reference a revision of this pipeline'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER valid_current_revision BEFORE INSERT OR UPDATE OF current_version ON pipelines
FOR EACH ROW EXECUTE FUNCTION validate_current_revision();
CREATE TABLE runs (
    id uuid PRIMARY KEY,
    pipeline_id uuid NOT NULL,
    version bigint NOT NULL,
    submitted_by uuid NOT NULL REFERENCES users(id),
    request_id uuid NOT NULL,
    request_hash text NOT NULL,
    definition jsonb NOT NULL,
    source_snapshot jsonb NOT NULL,
    input_hash text NOT NULL,
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','running','succeeded','failed','canceled')),
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    FOREIGN KEY (pipeline_id,version) REFERENCES pipeline_revisions(pipeline_id,version),
    UNIQUE(submitted_by,request_id)
);
CREATE TABLE job_runs (
    id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES runs(id),
    name text NOT NULL,
    position integer NOT NULL,
    definition jsonb NOT NULL,
    status text NOT NULL CHECK (status IN ('blocked','queued','assigned','running','finalizing','succeeded','failed','canceled','skipped','lost')),
    UNIQUE(run_id,name),
    UNIQUE(run_id,id)
);
CREATE TABLE job_dependencies (
    run_id uuid NOT NULL,
    job_id uuid NOT NULL,
    needs_id uuid NOT NULL,
    PRIMARY KEY(job_id,needs_id),
    CHECK(job_id <> needs_id),
    FOREIGN KEY(run_id,job_id) REFERENCES job_runs(run_id,id),
    FOREIGN KEY(run_id,needs_id) REFERENCES job_runs(run_id,id)
);
CREATE TABLE audit_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id uuid REFERENCES users(id),
    project_id uuid REFERENCES projects(id),
    action text NOT NULL,
    details jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX runs_history ON runs(pipeline_id,created_at DESC,id);
CREATE INDEX jobs_queue ON job_runs(status,run_id);
CREATE FUNCTION protect_run_inputs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.id,NEW.pipeline_id,NEW.version,NEW.submitted_by,NEW.request_id,NEW.request_hash,NEW.definition,NEW.source_snapshot,NEW.input_hash,NEW.created_at)
        IS DISTINCT FROM ROW(OLD.id,OLD.pipeline_id,OLD.version,OLD.submitted_by,OLD.request_id,OLD.request_hash,OLD.definition,OLD.source_snapshot,OLD.input_hash,OLD.created_at)
    THEN RAISE EXCEPTION 'run inputs are immutable'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_run_inputs BEFORE UPDATE ON runs
FOR EACH ROW EXECUTE FUNCTION protect_run_inputs();
CREATE FUNCTION protect_job_inputs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.id,NEW.run_id,NEW.name,NEW.position,NEW.definition)
        IS DISTINCT FROM ROW(OLD.id,OLD.run_id,OLD.name,OLD.position,OLD.definition)
    THEN RAISE EXCEPTION 'job inputs are immutable'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER immutable_job_inputs BEFORE UPDATE ON job_runs
FOR EACH ROW EXECUTE FUNCTION protect_job_inputs();
