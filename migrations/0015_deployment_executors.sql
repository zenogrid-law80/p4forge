-- Existing environments deliberately require an explicit administrator binding.
ALTER TABLE deployment_environments ADD COLUMN executor_agent_id uuid REFERENCES agents(id);
-- Preserve the reviewed executor on the approval, even after later environment edits.
ALTER TABLE run_deployments ADD COLUMN executor_agent_id uuid REFERENCES agents(id);
