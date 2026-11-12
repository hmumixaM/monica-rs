-- Source originals share immutable physical content, while references remain
-- connection-scoped so revision authorization never relies on a guessed path.
CREATE TABLE crm.source_blob_refs (
 connection_id uuid NOT NULL REFERENCES crm.source_connections(id),
 provider text NOT NULL CHECK(provider IN ('mail','whatsapp','apple_messages','apple_contacts')),
 sha256 text NOT NULL CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 media_id uuid NOT NULL REFERENCES crm.media_assets(id),byte_length bigint NOT NULL CHECK(byte_length>=0),
 created_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(connection_id,sha256)
);
CREATE TABLE crm.mail_classifications (
 connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,
 header_hash text NOT NULL,category text NOT NULL CHECK(category IN ('personal','transactional','marketing','pending')),
 classified_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(connection_id,external_id)
);
-- Account-fair journal recovery is independent of source collection leases.
CREATE TABLE crm.source_recovery_turns (
 connection_id uuid PRIMARY KEY REFERENCES crm.source_connections(id),
 served_at timestamptz NOT NULL
);
