ALTER TABLE crm.auth_login_attempts ADD COLUMN blocked_until timestamptz;
-- Match the public audit contract: audit IDs are opaque UUIDs, not counters.
ALTER TABLE crm.audit_events ALTER COLUMN id DROP IDENTITY;
ALTER TABLE crm.audit_events ALTER COLUMN id TYPE uuid USING gen_random_uuid();
ALTER TABLE crm.audit_events ALTER COLUMN id SET DEFAULT gen_random_uuid();
