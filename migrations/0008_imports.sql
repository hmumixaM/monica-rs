CREATE TABLE crm.import_service_tokens (
 id uuid PRIMARY KEY,source text NOT NULL,label text NOT NULL,token_hash text NOT NULL UNIQUE,
 created_at timestamptz NOT NULL DEFAULT now(),last_used_at timestamptz,revoked_at timestamptz
);
CREATE TABLE crm.import_request_nonces(nonce text PRIMARY KEY,expires_at timestamptz NOT NULL);
CREATE INDEX ON crm.import_request_nonces(expires_at);
CREATE TABLE crm.import_batches (
 id uuid PRIMARY KEY,source text NOT NULL,stream text NOT NULL,schema_version integer NOT NULL,
 batch_id text NOT NULL,idempotency_key text NOT NULL UNIQUE,body_hash text NOT NULL,
 cursor_before text,cursor_after text,observed_at timestamptz NOT NULL,
 status text NOT NULL DEFAULT 'received',received_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz,
 inserted_count bigint NOT NULL DEFAULT 0,changed_count bigint NOT NULL DEFAULT 0,unchanged_count bigint NOT NULL DEFAULT 0,
 processed_count bigint NOT NULL DEFAULT 0,record_count bigint NOT NULL,error_summary text,
 retry_after timestamptz NOT NULL DEFAULT now(),failure_count integer NOT NULL DEFAULT 0,
 UNIQUE(source,stream,batch_id)
);
-- A committed receipt owns the immutable request; retries resume its bounded pages.
CREATE TABLE crm.import_batch_inputs(batch_id uuid PRIMARY KEY REFERENCES crm.import_batches(id),payload jsonb NOT NULL);
CREATE TABLE crm.source_cursors(source text NOT NULL,stream text NOT NULL,cursor text,batch_id uuid REFERENCES crm.import_batches(id),updated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(source,stream));
CREATE TABLE crm.source_checkpoints(source text NOT NULL,stream text NOT NULL,file_id text NOT NULL,checksum text NOT NULL,record_offset bigint NOT NULL DEFAULT 0,updated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(source,stream,file_id));
