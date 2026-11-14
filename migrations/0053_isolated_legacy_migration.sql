-- Reading a saved analysis must not require permission to invoke a model.
ALTER TABLE crm.ai_settings ADD COLUMN execution_enabled boolean NOT NULL DEFAULT true;

-- Source field history can outlive its human-confirmed person association.
ALTER TABLE crm.field_revisions ALTER COLUMN profile_id DROP NOT NULL;
ALTER TABLE crm.field_revisions ADD CONSTRAINT field_revision_owner
 CHECK(profile_id IS NOT NULL OR source_account_id IS NOT NULL);

CREATE TABLE crm.legacy_migrations (
 id uuid PRIMARY KEY,manifest_hash text NOT NULL UNIQUE,
 source_manifest jsonb NOT NULL,status text NOT NULL DEFAULT 'importing',
 created_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz
);
CREATE TABLE crm.legacy_migration_tables (
 migration_id uuid NOT NULL REFERENCES crm.legacy_migrations(id),
 store text NOT NULL,table_name text NOT NULL,mode text NOT NULL,
 expected bigint NOT NULL,processed bigint NOT NULL DEFAULT 0,
 after_key jsonb,digest text,status text NOT NULL DEFAULT 'pending',
 PRIMARY KEY(migration_id,store,table_name)
);
-- History without an executable Rust equivalent remains pageable evidence.
-- Operational credentials and live sessions are explicitly excluded.
CREATE TABLE crm.legacy_history (
 migration_id uuid NOT NULL REFERENCES crm.legacy_migrations(id),
 store text NOT NULL,table_name text NOT NULL,record_key text NOT NULL,
 payload jsonb NOT NULL,PRIMARY KEY(migration_id,store,table_name,record_key)
);
CREATE TABLE crm.legacy_id_map (
 migration_id uuid NOT NULL REFERENCES crm.legacy_migrations(id),
 kind text NOT NULL,old_id text NOT NULL,new_id uuid NOT NULL,
 PRIMARY KEY(migration_id,kind,old_id)
);
DO $$DECLARE p text;BEGIN
 FOREACH p IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
  EXECUTE format('CREATE TABLE %I.account_identifiers(identifier text PRIMARY KEY,account_id uuid NOT NULL REFERENCES %I.accounts(id))',p,p);
  EXECUTE format('CREATE TABLE %I.media(sha256 text PRIMARY KEY,media_type text NOT NULL,byte_length bigint NOT NULL,object_path text NOT NULL)',p);
  EXECUTE format('CREATE TABLE %I.media_links(object_id uuid NOT NULL REFERENCES %I.objects(id),media_hash text NOT NULL REFERENCES %I.media(sha256),role text NOT NULL,PRIMARY KEY(object_id,media_hash,role))',p,p,p);
  EXECUTE format('ALTER TABLE %I.observations ADD COLUMN observed_at timestamptz',p);
 END LOOP;
END $$;
