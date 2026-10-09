-- Stable keyset pagination across all projects, including identical timestamps.
CREATE INDEX runs_workspace_history ON runs(created_at DESC,id DESC);
