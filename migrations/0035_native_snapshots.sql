-- Complete native contact snapshots are durable across bounded request pages.
CREATE TABLE crm.native_snapshots (
 connection_id uuid NOT NULL REFERENCES crm.source_connections(id),stream text NOT NULL,snapshot_id uuid NOT NULL,
 state text NOT NULL CHECK(state IN ('collecting','finalizing','complete','superseded')),
 generation bigint NOT NULL,after_external_id text NOT NULL DEFAULT '',created_at timestamptz NOT NULL DEFAULT now(),completed_at timestamptz,
 PRIMARY KEY(connection_id,stream,snapshot_id)
);
CREATE UNIQUE INDEX native_snapshot_active ON crm.native_snapshots(connection_id,stream) WHERE state IN ('collecting','finalizing');
CREATE INDEX native_snapshot_pending ON crm.native_snapshots(created_at) WHERE state='finalizing';
CREATE TABLE crm.native_snapshot_seen (
 connection_id uuid NOT NULL,stream text NOT NULL,snapshot_id uuid NOT NULL,external_id text NOT NULL,
 PRIMARY KEY(connection_id,stream,snapshot_id,external_id),
 FOREIGN KEY(connection_id,stream,snapshot_id) REFERENCES crm.native_snapshots(connection_id,stream,snapshot_id) ON DELETE CASCADE
);
