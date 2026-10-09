CREATE TABLE run_artifact_inputs (
    run_id uuid NOT NULL REFERENCES runs(id),
    artifact_id uuid NOT NULL REFERENCES artifacts(id),
    source_run_id uuid NOT NULL REFERENCES runs(id),
    name text NOT NULL,
    size bigint NOT NULL CHECK (size BETWEEN 0 AND 67108864),
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY(run_id,name),
    UNIQUE(run_id,artifact_id),
    CHECK (run_id<>source_run_id)
);
CREATE INDEX artifact_input_consumers ON run_artifact_inputs(artifact_id);
CREATE FUNCTION protect_artifact_input() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'run artifact inputs are immutable';
END;
$$;
CREATE TRIGGER immutable_artifact_input BEFORE UPDATE OR DELETE ON run_artifact_inputs
FOR EACH ROW EXECUTE FUNCTION protect_artifact_input();
