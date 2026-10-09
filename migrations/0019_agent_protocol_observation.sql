-- Last authenticated claim capability, used only for dispatch diagnostics.
ALTER TABLE agents ADD COLUMN last_protocol_version bigint;
ALTER TABLE agents ADD COLUMN protocol_reported_at timestamptz;
