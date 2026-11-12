-- Stateless browser transport configuration. Browser transcripts are never stored.
ALTER TABLE crm.ai_settings ADD COLUMN qa_enabled boolean NOT NULL DEFAULT true,
 ADD COLUMN context_window integer NOT NULL DEFAULT 32768 CHECK(context_window BETWEEN 1024 AND 2097152),
 ADD COLUMN max_tokens integer NOT NULL DEFAULT 8192 CHECK(max_tokens BETWEEN 1 AND 131072);
ALTER TABLE crm.mail_classifications ADD COLUMN source_hash text,ADD COLUMN revision bigint,ADD COLUMN model text;
CREATE TABLE crm.mail_event_extractions (
 evidence_id uuid PRIMARY KEY REFERENCES crm.connection_evidence(id),input_hash text NOT NULL,revision bigint NOT NULL,
 model text NOT NULL,events jsonb NOT NULL,job_id uuid REFERENCES crm.ai_jobs(id),generated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE crm.ai_mail_batches (
 id uuid PRIMARY KEY,connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_ids text[] NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),CHECK(cardinality(external_ids) BETWEEN 1 AND 25)
);
CREATE TABLE crm.memory_heads (
 profile_id uuid PRIMARY KEY REFERENCES crm.profiles(id),generation bigint NOT NULL DEFAULT 0,updated_at timestamptz NOT NULL DEFAULT now()
);
-- Materialize only current model-visible inputs; source revisions remain in
-- their source schemas. Stable source keys keep unrelated radix branches still.
CREATE TABLE crm.memory_inputs (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),source_key text NOT NULL,source_kind text NOT NULL,source_id text NOT NULL,
 bucket text NOT NULL,payload jsonb NOT NULL,input_hash text NOT NULL,private boolean NOT NULL,payload_bytes integer NOT NULL,
 PRIMARY KEY(profile_id,source_key)
);
CREATE INDEX ON crm.memory_inputs(profile_id,bucket,source_key);
CREATE INDEX ON crm.memory_inputs(profile_id,source_kind,source_id);
CREATE TABLE crm.memory_bucket_outputs (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),bucket text NOT NULL,signature text NOT NULL,model text NOT NULL,
 privacy_hash text NOT NULL,output jsonb NOT NULL,source_count bigint NOT NULL,
 PRIMARY KEY(profile_id,bucket)
);
CREATE TABLE crm.memory_parts (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),node text NOT NULL,input_hash text NOT NULL,model text NOT NULL,privacy_hash text NOT NULL,
 output jsonb NOT NULL,generated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(profile_id,node,input_hash,model,privacy_hash)
);
CREATE TABLE crm.memory_current (
 profile_id uuid PRIMARY KEY REFERENCES crm.profiles(id),generation bigint NOT NULL,input_hash text NOT NULL,
 model text NOT NULL,privacy_hash text NOT NULL,output jsonb NOT NULL,generated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.memory_evidence_owners (
 evidence_id uuid NOT NULL REFERENCES crm.connection_evidence(id) ON DELETE CASCADE,profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 source_id text NOT NULL,PRIMARY KEY(evidence_id,profile_id)
);
