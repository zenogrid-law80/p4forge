-- Preserve identity, credentials and execution history while removing ownership.
INSERT INTO audit_events(project_id,action,details)
SELECT project_id,'agent.made_global',jsonb_build_object('agent_id',id,'previous_project_id',project_id)
FROM agents;

ALTER TABLE agents DROP COLUMN project_id;
