ALTER TABLE crm.legacy_id_map ALTER COLUMN new_id TYPE text USING new_id::text;
DO $$DECLARE p text;BEGIN
 FOREACH p IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
  EXECUTE format('ALTER TABLE %I.revision_metadata ADD COLUMN source_event text',p);
 END LOOP;
END $$;
CREATE TABLE crm.legacy_message_evidence (
 message_id uuid PRIMARY KEY REFERENCES crm.message_search(message_id),
 source_record_id text,content_object_id uuid REFERENCES crm.content_objects(id),
 raw_object_id uuid REFERENCES crm.content_objects(id),original jsonb NOT NULL
);
CREATE TABLE crm.legacy_social_versions (
 id uuid PRIMARY KEY,event_id uuid NOT NULL REFERENCES crm.timeline_events(id),
 content_object_id uuid NOT NULL REFERENCES crm.content_objects(id),
 content_hash text NOT NULL,observed_at timestamptz NOT NULL,original jsonb NOT NULL
);
CREATE INDEX legacy_social_versions_event ON crm.legacy_social_versions(event_id,observed_at,id);
CREATE TABLE crm.source_relationship_edges (
 id uuid PRIMARY KEY,from_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),
 to_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),relationship_type text NOT NULL,
 note text,confidence double precision,valid_from timestamptz,valid_to timestamptz,
 source_type text NOT NULL,evidence_kind text NOT NULL
);
CREATE VIEW crm.presented_relationship_edges AS
 SELECT id,from_profile_id,to_profile_id,relationship_type,note,valid_from,valid_to,source_type,confidence,created_at,updated_at FROM crm.relationship_edges
 UNION ALL
 SELECT e.id,a.profile_id,b.profile_id,e.relationship_type,e.note,e.valid_from,e.valid_to,e.source_type,e.confidence,
 greatest(a.linked_at,b.linked_at),greatest(a.linked_at,b.linked_at)
 FROM crm.source_relationship_edges e JOIN crm.source_account_links a ON a.account_id=e.from_account_id
 JOIN crm.source_account_links b ON b.account_id=e.to_account_id
 WHERE a.profile_id<>b.profile_id AND NOT EXISTS(SELECT 1 FROM crm.relationship_edges m WHERE m.id=e.id);
