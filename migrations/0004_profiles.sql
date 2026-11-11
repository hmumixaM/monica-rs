CREATE TABLE crm.profiles (
 id uuid PRIMARY KEY, profile_type text NOT NULL CHECK(profile_type IN ('person','group')),
 display_name text NOT NULL CHECK(length(display_name) BETWEEN 1 AND 255), summary text,
 avatar_media_id uuid, created_at timestamptz NOT NULL DEFAULT now(), archived_at timestamptz,
 generation bigint NOT NULL DEFAULT 1
);
CREATE TABLE crm.field_revisions (
 id uuid PRIMARY KEY, revision_seq bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
 profile_id uuid NOT NULL REFERENCES crm.profiles(id), field_key text NOT NULL,
 value jsonb NOT NULL, content_hash text NOT NULL, source_type text NOT NULL,
 operation text NOT NULL, observed_at timestamptz NOT NULL DEFAULT now(),
 is_conflict boolean NOT NULL DEFAULT false, previous_revision_id uuid REFERENCES crm.field_revisions(id)
);
CREATE INDEX ON crm.field_revisions(profile_id,revision_seq DESC);
CREATE TABLE crm.field_current (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id), field_key text NOT NULL,
 revision_id uuid NOT NULL REFERENCES crm.field_revisions(id),
 PRIMARY KEY(profile_id,field_key)
);
CREATE TABLE crm.tags (id uuid PRIMARY KEY,name text NOT NULL UNIQUE CHECK(length(name) BETWEEN 1 AND 120));
CREATE TABLE crm.tag_memberships (
 tag_id uuid NOT NULL REFERENCES crm.tags(id),profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 source_type text NOT NULL DEFAULT 'manual',PRIMARY KEY(tag_id,profile_id)
);
CREATE TABLE crm.tag_membership_removals (
 tag_id uuid NOT NULL REFERENCES crm.tags(id),profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 removed_by uuid REFERENCES crm.app_users(id),removed_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(tag_id,profile_id)
);
CREATE TABLE crm.tag_membership_revisions (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tag_id uuid NOT NULL REFERENCES crm.tags(id),profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 operation text NOT NULL,source_type text NOT NULL DEFAULT 'manual',actor_user_id uuid REFERENCES crm.app_users(id),
 created_at timestamptz NOT NULL DEFAULT now()
);
