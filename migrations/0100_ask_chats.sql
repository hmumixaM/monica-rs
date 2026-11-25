CREATE TABLE crm.ask_chats (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES crm.app_users(id) ON DELETE CASCADE,
    title text NOT NULL DEFAULT '新会话',
    people jsonb NOT NULL DEFAULT '[]',
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ask_chats_owner ON crm.ask_chats (owner_id, updated_at DESC, id);

CREATE TABLE crm.ask_runs (
    id uuid PRIMARY KEY,
    chat_id uuid NOT NULL REFERENCES crm.ask_chats(id) ON DELETE CASCADE,
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','running','done','failed','cancelled')),
    progress text NOT NULL DEFAULT '正在排队…',
    error text NOT NULL DEFAULT '',
    model text NOT NULL,
    timezone text NOT NULL,
    people jsonb NOT NULL,
    checkpoint jsonb NOT NULL DEFAULT '{}',
    draft jsonb,
    lease_id uuid,
    lease_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ask_runs_one_active ON crm.ask_runs (chat_id) WHERE status IN ('queued','running');
CREATE INDEX ask_runs_queue ON crm.ask_runs (created_at) WHERE status IN ('queued','running');
CREATE INDEX ask_runs_recent ON crm.ask_runs (chat_id, created_at DESC, id);

CREATE TABLE crm.ask_messages (
    seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    chat_id uuid NOT NULL REFERENCES crm.ask_chats(id) ON DELETE CASCADE,
    run_id uuid REFERENCES crm.ask_runs(id) ON DELETE CASCADE,
    message jsonb NOT NULL
);
CREATE INDEX ask_messages_chat ON crm.ask_messages (chat_id, seq);
