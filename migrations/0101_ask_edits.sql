CREATE TABLE crm.ask_edits (
    id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES crm.ask_runs(id) ON DELETE CASCADE,
    tool_call_id text NOT NULL,
    action jsonb NOT NULL,
    preview jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','applied','dismissed','stale')),
    result jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (run_id, tool_call_id)
);
