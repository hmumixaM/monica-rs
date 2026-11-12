-- Source observations retain their chronology while identical A -> B -> A payloads
-- occupy one content-addressed row. This is a new empty installation migration.
DO $$ DECLARE source_schema text; BEGIN
  FOREACH source_schema IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
    EXECUTE format('INSERT INTO %I.payloads(hash,value) SELECT hash,payload FROM %I.revisions ON CONFLICT DO NOTHING',source_schema,source_schema);
    EXECUTE format('ALTER TABLE %I.revisions ADD CONSTRAINT revision_payload_reference FOREIGN KEY(hash) REFERENCES %I.payloads(hash)',source_schema,source_schema);
    EXECUTE format('ALTER TABLE %I.revisions DROP COLUMN payload',source_schema);
  END LOOP;
END $$;
-- This table is a fenced progress marker; business projections have typed tables.
ALTER TABLE crm.projections DROP COLUMN payload;
CREATE TABLE crm.source_avatar_projections (
 profile_id uuid PRIMARY KEY REFERENCES crm.profiles(id),
 media_id uuid NOT NULL REFERENCES crm.media_assets(id),
 source_account_id uuid NOT NULL REFERENCES crm.source_accounts(id)
);
ALTER TABLE crm.profile_redirects ADD COLUMN evidence jsonb NOT NULL DEFAULT '{}';
