ALTER TABLE crm.timeline_events ADD COLUMN connection_id uuid REFERENCES crm.source_connections(id);
CREATE UNIQUE INDEX connection_timeline_external ON crm.timeline_events(connection_id,event_type,external_id) WHERE connection_id IS NOT NULL;
CREATE INDEX ON crm.timeline_events(source_account_id,id);
CREATE INDEX ON crm.conversations(source_account_id,id);
CREATE TABLE crm.connection_social_revisions (
 event_id uuid NOT NULL REFERENCES crm.timeline_events(id),revision bigint NOT NULL,
 content_hash text NOT NULL,content_object_id uuid NOT NULL REFERENCES crm.content_objects(id),observed_at timestamptz NOT NULL,source_record_id text NOT NULL,
 PRIMARY KEY(event_id,revision)
);
