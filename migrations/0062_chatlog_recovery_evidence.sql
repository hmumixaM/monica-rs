-- A revoke can refer to an older shard not processed yet. Resolve only after
-- the full frozen snapshot has been consumed, preserving the notice itself.
CREATE TABLE crm.legacy_chatlog_revocations (
 notice_message_id uuid NOT NULL REFERENCES crm.message_search(message_id),
 conversation_id uuid NOT NULL REFERENCES crm.conversations(id),target_record_id text NOT NULL,
 content_object_id uuid NOT NULL REFERENCES crm.content_objects(id),
 snapshot_id uuid NOT NULL REFERENCES crm.legacy_chatlog_snapshots(id),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','applied','source_missing')),
 PRIMARY KEY(snapshot_id,notice_message_id)
);
CREATE TABLE crm.legacy_chatlog_issues (
 snapshot_id uuid NOT NULL REFERENCES crm.legacy_chatlog_snapshots(id),issue_key text NOT NULL,
 kind text NOT NULL,entity_id uuid,status text NOT NULL CHECK(status IN ('pending','source_missing','resolved')),
 details jsonb NOT NULL DEFAULT '{}',PRIMARY KEY(snapshot_id,issue_key)
);
