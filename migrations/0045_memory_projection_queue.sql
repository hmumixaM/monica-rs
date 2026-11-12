-- Independent social projections never wait for a person's Memory reduction.
-- Capture both old and current owners so reattribution/deletion also fences work.
CREATE TABLE crm.memory_legacy_pending_records (
 kind text NOT NULL CHECK(kind IN ('message','social','activity')),
 entity_id uuid NOT NULL,profiles uuid[] NOT NULL,
 requested_at timestamptz NOT NULL DEFAULT clock_timestamp(),PRIMARY KEY(kind,entity_id)
);
CREATE INDEX ON crm.memory_legacy_pending_records USING gin(profiles);
CREATE VIEW crm.memory_legacy_pending AS
 SELECT unnest(profiles) profile_id FROM crm.memory_legacy_scopes
 UNION ALL SELECT unnest(profiles) FROM crm.memory_legacy_pending_records;
