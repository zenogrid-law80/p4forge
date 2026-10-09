-- Unknown until the execution account reports a real Docker engine check.
ALTER TABLE agents
    ADD COLUMN docker_state text NOT NULL DEFAULT 'unknown'
        CHECK (docker_state IN ('unknown', 'available', 'cli_missing', 'engine_unavailable', 'check_failed')),
    ADD COLUMN docker_engine_os text CHECK (docker_engine_os IN ('linux', 'windows')),
    ADD COLUMN docker_checked_at timestamptz,
    ADD CONSTRAINT agent_docker_engine_state CHECK (
        (docker_state = 'available' AND docker_engine_os IS NOT NULL)
        OR (docker_state <> 'available' AND docker_engine_os IS NULL)
    );
