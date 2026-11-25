CREATE SEQUENCE crm.ask_edit_changes AS bigint;

CREATE TABLE crm.ask_edits (
    id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES crm.ask_runs(id) ON DELETE CASCADE,
    tool_call_hash text NOT NULL CHECK (length(tool_call_hash) = 64),
    action jsonb NOT NULL,
    preview jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','applied','dismissed','stale')),
    result jsonb,
    change_seq bigint NOT NULL DEFAULT nextval('crm.ask_edit_changes'),
    UNIQUE (run_id, tool_call_hash)
);
ALTER SEQUENCE crm.ask_edit_changes OWNED BY crm.ask_edits.change_seq;
CREATE INDEX ask_edits_run_changes ON crm.ask_edits (run_id, change_seq);
