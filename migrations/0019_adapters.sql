CREATE TABLE crm.linkedin_collection_targets (
 account_id uuid PRIMARY KEY REFERENCES crm.source_accounts(id),created_by uuid REFERENCES crm.app_users(id),created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.adapter_controls (
 provider text PRIMARY KEY CHECK(provider IN ('wechat','instagram','linkedin','linkedin_messages','chatlog')),
 enabled boolean NOT NULL DEFAULT false,paused boolean NOT NULL DEFAULT false,
 minimum_interval_seconds integer NOT NULL DEFAULT 60 CHECK(minimum_interval_seconds BETWEEN 1 AND 86400),
 updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO crm.adapter_controls(provider) VALUES('wechat'),('instagram'),('linkedin'),('chatlog');
-- Effective admission also requires the parent LinkedIn source to be enabled.
INSERT INTO crm.adapter_controls(provider,enabled) VALUES('linkedin_messages',true);
-- Published local snapshots incur no remote platform polling.
UPDATE crm.adapter_controls SET minimum_interval_seconds=1 WHERE provider='wechat';
CREATE TABLE crm.adapter_runs (
 id uuid PRIMARY KEY,provider text NOT NULL REFERENCES crm.adapter_controls(provider),
 status text NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','complete','failed','cancelled')),
 token uuid,lease_until timestamptz,attempts integer NOT NULL DEFAULT 0,
 progress jsonb NOT NULL DEFAULT '{}',cancel_requested boolean NOT NULL DEFAULT false,
 error text,created_at timestamptz NOT NULL DEFAULT now(),started_at timestamptz,completed_at timestamptz,
 heartbeat_at timestamptz
);
CREATE UNIQUE INDEX ON crm.adapter_runs(provider) WHERE status IN ('queued','running');
CREATE INDEX ON crm.adapter_runs(provider,created_at DESC);
CREATE TABLE crm.adapter_snapshots (
 provider text NOT NULL,external_id text NOT NULL,content_hash text NOT NULL,
 run_id uuid NOT NULL REFERENCES crm.adapter_runs(id),batch_id uuid NOT NULL REFERENCES crm.import_batches(id),
 published_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(provider,external_id)
);
CREATE TABLE crm.adapter_events (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,run_id uuid NOT NULL REFERENCES crm.adapter_runs(id),
 state text NOT NULL,details jsonb NOT NULL DEFAULT '{}',created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.adapter_media_inputs (
 id uuid PRIMARY KEY,run_id uuid NOT NULL REFERENCES crm.adapter_runs(id),provider text NOT NULL,
 path text NOT NULL,sha256 text NOT NULL CHECK(sha256 ~ '^[0-9a-f]{64}$'),byte_length bigint NOT NULL CHECK(byte_length>0),
 status text NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','complete','failed')),
 token uuid,lease_until timestamptz,error text,media_id uuid REFERENCES crm.media_assets(id),
 created_at timestamptz NOT NULL DEFAULT now(),UNIQUE(provider,sha256)
);
CREATE TABLE crm.adapter_spool_pages (
 provider text NOT NULL REFERENCES crm.adapter_controls(provider),page_id text NOT NULL,sha256 text NOT NULL,
 signature jsonb NOT NULL,byte_offset bigint NOT NULL DEFAULT 0 CHECK(byte_offset>=0),
 status text NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','complete','failed')),error text,
 run_id uuid REFERENCES crm.adapter_runs(id),updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(provider,page_id)
);
