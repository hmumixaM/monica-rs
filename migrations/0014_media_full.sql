ALTER TABLE crm.media_assets ADD COLUMN source_type text;
ALTER TABLE crm.media_assets ADD COLUMN source_record_id text;
ALTER TABLE crm.media_assets ADD COLUMN capture_at timestamptz;
ALTER TABLE crm.media_assets ADD COLUMN duration_ms bigint CHECK(duration_ms>=0);
ALTER TABLE crm.media_assets ADD COLUMN verified_at timestamptz;
ALTER TABLE crm.media_links ADD COLUMN source_account_id uuid REFERENCES crm.source_accounts(id);
CREATE INDEX media_links_asset ON crm.media_links(media_id);

CREATE TABLE crm.media_publications (
 sha256 text PRIMARY KEY CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 manifest jsonb NOT NULL, status text NOT NULL DEFAULT 'pending',
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 error text
);
CREATE TABLE crm.media_uploads (
 id uuid PRIMARY KEY, expected_sha256 text, media_type text NOT NULL,
 expected_byte_length bigint CHECK(expected_byte_length BETWEEN 0 AND 524288000),
 file_name text, created_by uuid NOT NULL REFERENCES crm.app_users(id),
 next_chunk integer NOT NULL DEFAULT 0,received_bytes bigint NOT NULL DEFAULT 0,
 status text NOT NULL DEFAULT 'uploading',media_id uuid REFERENCES crm.media_assets(id),
 created_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz,
 expires_at timestamptz NOT NULL DEFAULT now()+interval '7 days',
 CHECK(expected_sha256 IS NULL OR expected_sha256 ~ '^[0-9a-f]{64}$')
);
CREATE TABLE crm.media_upload_chunks (
 upload_id uuid NOT NULL REFERENCES crm.media_uploads(id) ON DELETE CASCADE,
 chunk integer NOT NULL CHECK(chunk>=0),sha256 text NOT NULL,byte_length bigint NOT NULL,
 object_path text NOT NULL, PRIMARY KEY(upload_id,chunk)
);
CREATE TABLE crm.media_provenance (
 media_id uuid NOT NULL REFERENCES crm.media_assets(id),source text NOT NULL,
 source_record_id text NOT NULL,source_url text,import_batch_id uuid REFERENCES crm.import_batches(id),
 created_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(media_id,source,source_record_id)
);
CREATE TABLE crm.media_slots (
 entity_type text NOT NULL,entity_id uuid NOT NULL,role text NOT NULL,
 media_id uuid NOT NULL REFERENCES crm.media_assets(id),generation bigint NOT NULL DEFAULT 1,
 source_observed_at timestamptz,source_batch_id uuid REFERENCES crm.import_batches(id),source_ordinal integer,
 updated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(entity_type,entity_id,role)
);
CREATE TABLE crm.media_import_progress (
 batch_id uuid PRIMARY KEY REFERENCES crm.import_batches(id),
 media_offset integer NOT NULL DEFAULT 0,raw_offset integer NOT NULL DEFAULT 0,
 complete boolean NOT NULL DEFAULT false,updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.import_raw_objects (
 batch_id uuid NOT NULL REFERENCES crm.import_batches(id),ordinal integer NOT NULL,
 sha256 text NOT NULL,payload jsonb NOT NULL,PRIMARY KEY(batch_id,ordinal)
);
CREATE TABLE crm.media_dependencies (
 id uuid PRIMARY KEY,import_batch_id uuid NOT NULL REFERENCES crm.import_batches(id),ordinal integer NOT NULL,
 provider text NOT NULL,record jsonb NOT NULL,state text NOT NULL DEFAULT 'waiting',
 reason text NOT NULL DEFAULT 'media_pending',media_id uuid REFERENCES crm.media_assets(id),
 attempts integer NOT NULL DEFAULT 0,next_attempt_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(),updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(import_batch_id,ordinal)
);
CREATE INDEX media_dependencies_due ON crm.media_dependencies(next_attempt_at,id) WHERE state='waiting';
CREATE TABLE crm.media_profile_refresh(account_id uuid PRIMARY KEY REFERENCES crm.source_accounts(id),generation bigint NOT NULL DEFAULT 1);
CREATE TABLE crm.media_maintenance (
 singleton boolean PRIMARY KEY CHECK(singleton),bucket integer NOT NULL DEFAULT 0,
 last_file text NOT NULL DEFAULT '',updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO crm.media_maintenance(singleton) VALUES(true);
