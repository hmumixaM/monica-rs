ALTER TABLE crm.import_batches ADD COLUMN connection_id uuid REFERENCES crm.source_connections(id);
ALTER TABLE crm.import_batches DROP CONSTRAINT import_batches_source_stream_batch_id_key;
CREATE UNIQUE INDEX import_legacy_batch ON crm.import_batches(source,stream,batch_id) WHERE connection_id IS NULL;
CREATE UNIQUE INDEX import_connection_batch ON crm.import_batches(connection_id,stream,batch_id) WHERE connection_id IS NOT NULL;
-- Signed import annotations are CRM-owned; arbitrary native payload profile IDs
-- never become identity authority. They bind the committed source revision only.
CREATE TABLE crm.connection_import_bindings (
 connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,revision bigint NOT NULL,
 profile_id uuid REFERENCES crm.profiles(id),sender_profile_id uuid REFERENCES crm.profiles(id),
 conversation_type text NOT NULL CHECK(conversation_type IN ('direct','group')),
 PRIMARY KEY(connection_id,external_id,revision)
);
