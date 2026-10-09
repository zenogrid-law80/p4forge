ALTER TABLE agents ADD COLUMN perforce_managed boolean NOT NULL DEFAULT false;
CREATE TABLE agent_perforce_credentials (
    agent_id uuid NOT NULL REFERENCES agents(id),
    credential_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('read','publish')),
    connection jsonb NOT NULL,
    server_id text NOT NULL,
    generation uuid NOT NULL,
    server_state text NOT NULL DEFAULT 'checking' CHECK (server_state IN ('checking','ready','error')),
    server_checked_at timestamptz,
    server_expires_at timestamptz,
    renewed_at timestamptz,
    agent_state text NOT NULL DEFAULT 'checking' CHECK (agent_state IN ('checking','ready','error')),
    agent_checked_at timestamptz,
    agent_expires_at timestamptz,
    PRIMARY KEY(agent_id,credential_id),
    UNIQUE(agent_id,role)
);
