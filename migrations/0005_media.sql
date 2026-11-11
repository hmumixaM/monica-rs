CREATE TABLE crm.media_assets (
 id uuid PRIMARY KEY, sha256 text NOT NULL UNIQUE CHECK(sha256 ~ '^[0-9a-f]{64}$'),
 media_type text NOT NULL, byte_length bigint NOT NULL CHECK(byte_length>=0),
 object_path text NOT NULL, file_name text, width integer,height integer,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE crm.media_links (
 entity_type text NOT NULL,entity_id uuid NOT NULL,media_id uuid NOT NULL REFERENCES crm.media_assets(id),
 role text NOT NULL,created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(entity_type,entity_id,media_id,role)
);
ALTER TABLE crm.profiles ADD FOREIGN KEY(avatar_media_id) REFERENCES crm.media_assets(id);
