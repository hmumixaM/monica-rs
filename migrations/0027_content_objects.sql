-- CRM content addresses retain the public legacy namespace, with a typed
-- fallback only when the unprefixed address is already occupied by another value.
CREATE TABLE crm.content_objects (
 id uuid PRIMARY KEY,content_kind text NOT NULL,sha256 text NOT NULL,
 byte_length bigint NOT NULL CHECK(byte_length>=0),payload_json jsonb NOT NULL,
 UNIQUE(content_kind,sha256)
);
ALTER TABLE crm.field_revisions ADD COLUMN content_object_id uuid REFERENCES crm.content_objects(id);
ALTER TABLE crm.source_fields ADD COLUMN content_object_id uuid REFERENCES crm.content_objects(id);
CREATE INDEX ON crm.field_revisions(content_hash);
CREATE FUNCTION crm.lock_content_addresses(keys text[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE address text;
BEGIN
 FOR address IN SELECT DISTINCT k FROM unnest(keys) k ORDER BY k LOOP
  PERFORM pg_advisory_xact_lock(hashtextextended(address,9723531));
 END LOOP;
END $$;

ALTER TABLE crm.source_structured_revisions ADD FOREIGN KEY(content_object_id) REFERENCES crm.content_objects(id);
