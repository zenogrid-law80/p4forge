ALTER TABLE agents ADD COLUMN binary_sha256 text;
ALTER TABLE agents ADD COLUMN agent_version text;
ALTER TABLE agents ADD COLUMN self_patch_ready boolean NOT NULL DEFAULT false;

CREATE TABLE agent_releases (
    id uuid PRIMARY KEY,
    version text NOT NULL,
    os text NOT NULL CHECK (os IN ('macos','windows')),
    arch text NOT NULL CHECK (arch IN ('aarch64','x86_64')),
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size bigint NOT NULL CHECK (size BETWEEN 1 AND 67108864),
    payload bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(os,arch,sha256),
    CHECK (octet_length(payload)=size)
);
CREATE TABLE agent_updates (
    id uuid PRIMARY KEY,
    agent_id uuid NOT NULL REFERENCES agents(id),
    release_id uuid NOT NULL REFERENCES agent_releases(id),
    previous_sha256 text NOT NULL,
    previous_draining boolean NOT NULL,
    status text NOT NULL DEFAULT 'requested' CHECK (status IN ('requested','staged','succeeded','rolled_back','failed','cancelled')),
    reason text NOT NULL DEFAULT '',
    requested_by uuid NOT NULL REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX agent_one_pending_update ON agent_updates(agent_id) WHERE status IN ('requested','staged');
