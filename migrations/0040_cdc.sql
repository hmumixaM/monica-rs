-- Optional WAL history is independent of all source/projection queues.
CREATE TABLE crm.cdc_consumers (
 provider text PRIMARY KEY,slot_name text NOT NULL,established boolean NOT NULL DEFAULT false,
 captured_lsn pg_lsn,retained_wal_bytes bigint NOT NULL DEFAULT 0,state text NOT NULL,
 error_code text,heartbeat_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.cdc_events (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,provider text NOT NULL,commit_lsn pg_lsn NOT NULL,
 ordinal integer NOT NULL,relation_name text NOT NULL,operation text NOT NULL CHECK(operation IN ('I','U','D','T')),
 before_row jsonb,after_row jsonb,captured_at timestamptz NOT NULL DEFAULT now(),committed_at timestamptz NOT NULL,
 UNIQUE(provider,commit_lsn,ordinal)
);
CREATE INDEX cdc_events_provider_id ON crm.cdc_events(provider,id DESC);

-- Queue age stays on small active rows; history preserves identical row layout.
ALTER TABLE crm.jobs ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE crm.job_history ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();
