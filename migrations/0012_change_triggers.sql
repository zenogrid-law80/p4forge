CREATE TABLE pipeline_triggers (
    pipeline_id uuid PRIMARY KEY REFERENCES pipelines(id),
    version bigint NOT NULL,
    generation bigint NOT NULL CHECK (generation > 0),
    enabled boolean NOT NULL DEFAULT false,
    includes jsonb NOT NULL,
    excludes jsonb NOT NULL,
    configured_by uuid NOT NULL REFERENCES users(id),
    source_binding jsonb NOT NULL,
    server_id text NOT NULL,
    cursor bigint NOT NULL CHECK (cursor >= 0),
    checked_at timestamptz,
    last_error text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (pipeline_id,version) REFERENCES pipeline_revisions(pipeline_id,version)
);
CREATE TABLE pipeline_trigger_events (
    pipeline_id uuid NOT NULL REFERENCES pipeline_triggers(pipeline_id),
    server_id text NOT NULL,
    changelist bigint NOT NULL CHECK (changelist > 0),
    generation bigint NOT NULL,
    version bigint NOT NULL,
    outcome text NOT NULL CHECK (outcome IN ('created','filtered')),
    run_id uuid UNIQUE REFERENCES runs(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (pipeline_id,server_id,changelist),
    CHECK ((outcome='created') = (run_id IS NOT NULL))
);
CREATE INDEX pipeline_trigger_events_recent ON pipeline_trigger_events(pipeline_id,created_at DESC);
