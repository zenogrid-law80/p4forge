CREATE TABLE deployment_environments (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id),
    pipeline_id uuid NOT NULL REFERENCES pipelines(id),
    name text NOT NULL,
    generation bigint NOT NULL DEFAULT 1,
    active_run_id uuid REFERENCES runs(id),
    current_run_id uuid REFERENCES runs(id),
    previous_run_id uuid REFERENCES runs(id),
    health text NOT NULL DEFAULT 'unknown' CHECK (health IN ('unknown','healthy')),
    checked_at timestamptz,
    UNIQUE(project_id,name)
);
CREATE TABLE run_deployments (
    run_id uuid PRIMARY KEY REFERENCES runs(id),
    environment_id uuid NOT NULL REFERENCES deployment_environments(id),
    status text NOT NULL DEFAULT 'awaiting_approval' CHECK (status IN
      ('awaiting_approval','approved','running','succeeded','rejected','canceled','recovery_required','resolved')),
    rollback_of uuid REFERENCES run_deployments(run_id),
    approved_by uuid REFERENCES users(id),
    approved_at timestamptz,
    note text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);
CREATE INDEX deployment_history ON run_deployments(environment_id,created_at DESC);
CREATE FUNCTION immutable_deployment_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'deployment history is immutable'; END IF;
  IF ROW(NEW.run_id,NEW.environment_id,NEW.rollback_of) IS DISTINCT FROM
     ROW(OLD.run_id,OLD.environment_id,OLD.rollback_of) THEN
    RAISE EXCEPTION 'deployment identity is immutable';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER deployment_identity BEFORE UPDATE OR DELETE ON run_deployments
FOR EACH ROW EXECUTE FUNCTION immutable_deployment_identity();
