CREATE TABLE configuration_imports (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id),
    source_name text NOT NULL,
    source_toml text NOT NULL CHECK (octet_length(source_toml) <= 262144),
    source_hash text NOT NULL,
    imported_by uuid NOT NULL REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(project_id,source_hash)
);
CREATE TABLE configuration_drafts (
    pipeline_id uuid PRIMARY KEY REFERENCES pipelines(id),
    import_id uuid NOT NULL REFERENCES configuration_imports(id),
    generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0),
    definition jsonb NOT NULL,
    edited_by uuid NOT NULL REFERENCES users(id),
    updated_at timestamptz NOT NULL DEFAULT now()
);
