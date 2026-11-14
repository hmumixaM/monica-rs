CREATE INDEX legacy_message_source_record ON crm.legacy_message_evidence(source_record_id,message_id);
CREATE TABLE crm.legacy_chatlog_snapshots (
 id uuid PRIMARY KEY,manifest jsonb NOT NULL,own_id text NOT NULL,status text NOT NULL DEFAULT 'importing',
 records bigint NOT NULL DEFAULT 0,inserted bigint NOT NULL DEFAULT 0,unchanged bigint NOT NULL DEFAULT 0,
 changed bigint NOT NULL DEFAULT 0,created_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz
);
-- One atomic receipt per bounded, content-addressed Reader page.
CREATE TABLE crm.legacy_chatlog_pages (
 snapshot_id uuid NOT NULL REFERENCES crm.legacy_chatlog_snapshots(id),
 digest text NOT NULL,records bigint NOT NULL,PRIMARY KEY(snapshot_id,digest)
);
CREATE TABLE crm.legacy_chatlog_versions (
 entity_type text NOT NULL CHECK(entity_type IN ('message','post','contact')),entity_id uuid NOT NULL,
 revision bigint NOT NULL,content_object_id uuid NOT NULL REFERENCES crm.content_objects(id),
 saved_row jsonb NOT NULL,snapshot_id uuid NOT NULL REFERENCES crm.legacy_chatlog_snapshots(id),
 created_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(entity_type,entity_id,revision)
);
-- Preserve the original CRM presentation before applying a newer Chatlog
-- snapshot. This makes the source-copy verifier account for every correction.
CREATE TABLE crm.legacy_chatlog_projection (
 entity_type text NOT NULL,entity_id uuid NOT NULL,original_row jsonb NOT NULL,
 current_row jsonb NOT NULL,revision bigint NOT NULL,
 PRIMARY KEY(entity_type,entity_id)
);
