-- Source-owned facts have no CRM person foreign keys. All names are qualified.
DO $$ DECLARE s text; BEGIN
 FOREACH s IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
  EXECUTE format('ALTER TABLE %I.accounts ADD COLUMN object_kind text NOT NULL DEFAULT ''person'', ADD COLUMN display_name text, ADD COLUMN username text, ADD COLUMN profile_url text',s);
  EXECUTE format('CREATE TABLE %I.payloads(hash text PRIMARY KEY,value jsonb NOT NULL)',s);
  EXECUTE format('INSERT INTO %I.payloads SELECT DISTINCT hash,payload FROM %I.revisions ON CONFLICT DO NOTHING',s,s);
  EXECUTE format('CREATE TABLE %I.revision_metadata(object_id uuid NOT NULL,version bigint NOT NULL,occurred_at timestamptz,observed_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(object_id,version),FOREIGN KEY(object_id,version) REFERENCES %I.revisions(object_id,version))',s,s);
  EXECUTE format('CREATE TABLE %I.message_corrections(account text NOT NULL,conversation text NOT NULL,external_id text NOT NULL,event_id text NOT NULL,body text,tombstone boolean NOT NULL DEFAULT false,created_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(account,conversation,external_id,event_id),FOREIGN KEY(account,conversation,external_id) REFERENCES %I.messages(account,conversation,external_id))',s,s);
  EXECUTE format('CREATE TABLE %I.events(account text NOT NULL,kind text NOT NULL,external_id text NOT NULL,occurred_at timestamptz,content jsonb NOT NULL,hash text NOT NULL,PRIMARY KEY(account,kind,external_id))',s);
 END LOOP;
END $$;
CREATE TABLE crm.source_accounts (
 id uuid PRIMARY KEY,provider text NOT NULL CHECK(provider IN ('wechat','instagram','linkedin')),
 external_id text NOT NULL,object_kind text NOT NULL CHECK(object_kind IN ('person','group')),
 display_name text NOT NULL,username text,profile_url text,last_seen_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(provider,external_id)
);
CREATE TABLE crm.source_account_links (
 account_id uuid PRIMARY KEY REFERENCES crm.source_accounts(id),profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 generation bigint NOT NULL DEFAULT 1,linked_at timestamptz NOT NULL DEFAULT now(),linked_by uuid REFERENCES crm.app_users(id)
);
CREATE INDEX ON crm.source_account_links(profile_id);
CREATE TABLE crm.source_fields (
 account_id uuid NOT NULL REFERENCES crm.source_accounts(id),field_key text NOT NULL,
 value jsonb NOT NULL,content_hash text NOT NULL,object_id uuid NOT NULL,source_version bigint NOT NULL,
 observed_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(account_id,field_key)
);
ALTER TABLE crm.field_revisions ADD COLUMN source_account_id uuid REFERENCES crm.source_accounts(id);
CREATE INDEX ON crm.field_revisions(source_account_id,field_key,revision_seq DESC);
CREATE TABLE crm.source_group_memberships (
 group_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),person_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),
 role text NOT NULL DEFAULT 'member',joined_at timestamptz,left_at timestamptz,
 PRIMARY KEY(group_account_id,person_account_id)
);
CREATE TABLE crm.contact_events (
 id uuid PRIMARY KEY,provider text NOT NULL,source_account_id uuid REFERENCES crm.source_accounts(id),
 profile_id uuid REFERENCES crm.profiles(id),kind text NOT NULL CHECK(kind IN ('call','money','interaction')),
 external_id text NOT NULL,occurred_at timestamptz,content jsonb NOT NULL,
 UNIQUE(provider,source_account_id,kind,external_id)
);
CREATE INDEX ON crm.contact_events(profile_id,kind,occurred_at DESC,id DESC);
CREATE TABLE crm.conversations (
 id uuid PRIMARY KEY,provider text NOT NULL,account text NOT NULL,external_id text NOT NULL,
 source_account_id uuid REFERENCES crm.source_accounts(id),profile_id uuid REFERENCES crm.profiles(id),
 conversation_type text NOT NULL CHECK(conversation_type IN ('direct','group')),
 first_message_at timestamptz,last_message_at timestamptz,last_message_id uuid,last_message_preview text,message_count bigint NOT NULL DEFAULT 0,
 UNIQUE(provider,account,external_id)
);
CREATE INDEX ON crm.conversations(profile_id,last_message_at DESC,id DESC);
CREATE TABLE crm.message_search (
 message_id uuid PRIMARY KEY,conversation_id uuid NOT NULL REFERENCES crm.conversations(id),
 provider text NOT NULL,account text NOT NULL,external_id text NOT NULL,sender text NOT NULL,
 sender_account_id uuid REFERENCES crm.source_accounts(id),occurred_at timestamptz NOT NULL,
 body text NOT NULL,message_type text NOT NULL DEFAULT 'text',tombstone boolean NOT NULL DEFAULT false,
 UNIQUE(conversation_id,external_id)
);
CREATE INDEX ON crm.message_search(conversation_id,occurred_at DESC,message_id DESC) WHERE NOT tombstone;
CREATE INDEX ON crm.message_search USING gin(to_tsvector('simple',body));
CREATE TABLE crm.message_months (
 conversation_id uuid NOT NULL REFERENCES crm.conversations(id),month text NOT NULL,
 sent bigint NOT NULL DEFAULT 0,received bigint NOT NULL DEFAULT 0,first_message_at timestamptz,last_message_at timestamptz,
 PRIMARY KEY(conversation_id,month)
);
-- Dedicated typed facts. Generic notebook fields never become typed entities.
CREATE TABLE crm.organizations (
 id uuid PRIMARY KEY,kind text NOT NULL CHECK(kind IN ('company','school')),
 normalized_name text NOT NULL,name text NOT NULL,UNIQUE(kind,normalized_name)
);
CREATE TABLE crm.source_structured_revisions (
 id uuid PRIMARY KEY,revision_seq bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
 source_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),
 fact_type text NOT NULL CHECK(fact_type IN ('address','employment','education')),
 fact_key text NOT NULL,entity_id uuid REFERENCES crm.organizations(id),content_object_id uuid NOT NULL,content_hash text NOT NULL,
 value jsonb NOT NULL,source_type text NOT NULL,source_record_id text NOT NULL,
 object_id uuid NOT NULL,source_version bigint NOT NULL,observed_at timestamptz NOT NULL,
 previous_revision_id uuid REFERENCES crm.source_structured_revisions(id),
 is_conflict boolean NOT NULL DEFAULT false,position integer NOT NULL DEFAULT 0,
 UNIQUE(source_account_id,fact_type,fact_key,object_id,source_version)
);
CREATE INDEX ON crm.source_structured_revisions(source_account_id,fact_type,fact_key,revision_seq DESC);
CREATE TABLE crm.source_structured_current (
 source_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),fact_type text NOT NULL,fact_key text NOT NULL,
 revision_id uuid NOT NULL REFERENCES crm.source_structured_revisions(id),
 PRIMARY KEY(source_account_id,fact_type,fact_key)
);
CREATE TABLE crm.source_chat_months (
 account_id uuid NOT NULL REFERENCES crm.source_accounts(id),month text NOT NULL,
 sent bigint NOT NULL,received bigint NOT NULL,total bigint NOT NULL,
 first_message_at timestamptz,last_message_at timestamptz,snapshot_at timestamptz NOT NULL,
 timezone text NOT NULL DEFAULT 'UTC',PRIMARY KEY(account_id,month)
);
CREATE TABLE crm.message_months_utc (LIKE crm.message_months INCLUDING ALL);
