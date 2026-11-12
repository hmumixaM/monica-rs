-- All seven source stores are independent schemas in the single native database.
CREATE SCHEMA whatsapp;
CREATE SCHEMA apple_messages;
CREATE SCHEMA apple_contacts;
CREATE SCHEMA mail;
ALTER TABLE crm.source_accounts DROP CONSTRAINT source_accounts_provider_check;
ALTER TABLE crm.source_accounts ADD CHECK(provider IN ('wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail'));
ALTER TABLE crm.jobs DROP CONSTRAINT jobs_provider_check;
ALTER TABLE crm.jobs ADD CHECK(provider IN ('wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail'));
DO $$DECLARE s text;BEGIN
 FOREACH s IN ARRAY ARRAY['whatsapp','apple_messages','apple_contacts','mail'] LOOP
  EXECUTE format('CREATE TABLE %I.accounts(LIKE wechat.accounts INCLUDING ALL)',s);
  EXECUTE format('CREATE TABLE %I.objects(LIKE wechat.objects INCLUDING ALL)',s);
  EXECUTE format('ALTER TABLE %I.objects ADD FOREIGN KEY(account_id) REFERENCES %I.accounts(id)',s,s);
  EXECUTE format('CREATE TABLE %I.payloads(LIKE wechat.payloads INCLUDING ALL)',s);
  EXECUTE format('CREATE TABLE %I.revisions(LIKE wechat.revisions INCLUDING ALL)',s);
  EXECUTE format('ALTER TABLE %I.revisions ADD FOREIGN KEY(object_id) REFERENCES %I.objects(id),ADD FOREIGN KEY(hash) REFERENCES %I.payloads(hash)',s,s,s);
  EXECUTE format('CREATE TABLE %I.observations(LIKE wechat.observations INCLUDING ALL)',s);
  EXECUTE format('ALTER TABLE %I.observations ADD FOREIGN KEY(object_id,version) REFERENCES %I.revisions(object_id,version)',s,s);
  EXECUTE format('CREATE TABLE %I.revision_metadata(LIKE wechat.revision_metadata INCLUDING ALL)',s);
  EXECUTE format('CREATE TABLE %I.messages(LIKE wechat.messages INCLUDING ALL)',s);
  EXECUTE format('CREATE TABLE %I.message_corrections(LIKE wechat.message_corrections INCLUDING ALL)',s);
  EXECUTE format('CREATE TABLE %I.events(LIKE wechat.events INCLUDING ALL)',s);
 END LOOP;
 -- Connection payloads are immutable and shared by A -> B -> A revisions. Views
 -- expose the pinned journal shape without duplicating JSON in heads/history.
 FOREACH s IN ARRAY ARRAY['wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail'] LOOP
  EXECUTE format('CREATE TABLE %I.connection_payloads(content_hash text PRIMARY KEY,payload jsonb NOT NULL,byte_length bigint GENERATED ALWAYS AS (octet_length(payload::text)) STORED)',s);
  EXECUTE format('CREATE TABLE %I.connection_record_heads(connection_id uuid NOT NULL,external_id text NOT NULL,revision bigint NOT NULL,content_hash text NOT NULL REFERENCES %I.connection_payloads(content_hash),updated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(connection_id,external_id))',s,s);
  EXECUTE format('CREATE TABLE %I.connection_revision_refs(connection_id uuid NOT NULL,external_id text NOT NULL,revision bigint NOT NULL,content_hash text NOT NULL REFERENCES %I.connection_payloads(content_hash),observed_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(connection_id,external_id,revision))',s,s);
  EXECUTE format('CREATE VIEW %I.connection_records AS SELECT r.*,p.payload FROM %I.connection_record_heads r JOIN %I.connection_payloads p USING(content_hash)',s,s,s);
  EXECUTE format('CREATE VIEW %I.connection_revisions AS SELECT r.*,p.payload FROM %I.connection_revision_refs r JOIN %I.connection_payloads p USING(content_hash)',s,s,s);
  EXECUTE format('CREATE TABLE %I.connection_outbox(sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,connection_id uuid NOT NULL,external_id text NOT NULL,revision bigint NOT NULL,delivered_at timestamptz,UNIQUE(connection_id,external_id,revision),FOREIGN KEY(connection_id,external_id,revision) REFERENCES %I.connection_revision_refs(connection_id,external_id,revision))',s,s);
  EXECUTE format('CREATE INDEX ON %I.connection_outbox(connection_id,sequence) WHERE delivered_at IS NULL',s);
  EXECUTE format('CREATE TABLE %I.connection_cursors(connection_id uuid NOT NULL,stream text NOT NULL,cursor jsonb NOT NULL,PRIMARY KEY(connection_id,stream))',s);
 END LOOP;
END $$;
CREATE TABLE crm.source_connections(
 id uuid PRIMARY KEY,provider text NOT NULL CHECK(provider IN ('wechat','instagram','linkedin','whatsapp','apple_messages','apple_contacts','mail')),
 external_account text NOT NULL,display_name text NOT NULL,connection_method text NOT NULL,state text NOT NULL DEFAULT 'disconnected',
 settings jsonb NOT NULL DEFAULT '{}',credential_ciphertext text,enabled boolean NOT NULL DEFAULT false,
 next_sync_at timestamptz NOT NULL DEFAULT now(),last_success_at timestamptz,error text,
 lease_owner uuid,lease_token uuid,lease_until timestamptz,sync_pending boolean NOT NULL DEFAULT false,
 generation bigint NOT NULL DEFAULT 1,created_at timestamptz NOT NULL DEFAULT now(),updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(provider,external_account)
);
CREATE INDEX ON crm.source_connections(next_sync_at) WHERE enabled;
CREATE TABLE crm.source_connection_cursors(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),stream text NOT NULL,cursor jsonb NOT NULL DEFAULT '{}',updated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(connection_id,stream));
CREATE TABLE crm.person_identifiers(profile_id uuid NOT NULL REFERENCES crm.profiles(id),kind text NOT NULL CHECK(kind IN ('email','phone','source')),value text NOT NULL,confirmed boolean NOT NULL DEFAULT false,is_self boolean NOT NULL DEFAULT false,PRIMARY KEY(profile_id,kind,value));
CREATE INDEX ON crm.person_identifiers(kind,value);
CREATE TABLE crm.connection_identifiers(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,profile_id uuid NOT NULL REFERENCES crm.profiles(id),kind text NOT NULL,value text NOT NULL,active boolean NOT NULL DEFAULT true,PRIMARY KEY(connection_id,external_id,kind,value));
CREATE INDEX ON crm.connection_identifiers(kind,value,profile_id) WHERE active;
CREATE TABLE crm.connection_identity_links(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,profile_id uuid REFERENCES crm.profiles(id),confirmed boolean NOT NULL DEFAULT false,blocked boolean NOT NULL DEFAULT false,PRIMARY KEY(connection_id,external_id),CHECK((blocked AND profile_id IS NULL)OR(NOT blocked AND profile_id IS NOT NULL)));
CREATE TABLE crm.connection_identity_candidates(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,profile_id uuid NOT NULL REFERENCES crm.profiles(id),reason text NOT NULL,PRIMARY KEY(connection_id,external_id,profile_id));
CREATE TABLE crm.connection_people(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,display_name text NOT NULL,is_self boolean NOT NULL DEFAULT false,resolved_profile_id uuid REFERENCES crm.profiles(id),PRIMARY KEY(connection_id,external_id));
ALTER TABLE crm.conversations ADD COLUMN connection_id uuid REFERENCES crm.source_connections(id),ADD COLUMN source_external_id text;
CREATE INDEX ON crm.conversations(provider,connection_id);
ALTER TABLE crm.message_search ADD COLUMN sender_profile_id uuid REFERENCES crm.profiles(id),ADD COLUMN direction text CHECK(direction IN ('sent','received','unknown'));
CREATE TABLE crm.connection_evidence(
 id uuid PRIMARY KEY,connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,revision bigint NOT NULL,content_hash text NOT NULL,
 kind text NOT NULL,occurred_at timestamptz,title text NOT NULL DEFAULT '',body text NOT NULL DEFAULT '',category text,included boolean NOT NULL,deleted boolean NOT NULL DEFAULT false,
 metadata jsonb NOT NULL DEFAULT '{}',message_id uuid,conversation_id uuid REFERENCES crm.conversations(id),dedup_key text,
 UNIQUE(connection_id,external_id)
);
CREATE INDEX ON crm.connection_evidence(occurred_at DESC,id DESC) WHERE included AND NOT deleted;
CREATE INDEX ON crm.connection_evidence USING gin(to_tsvector('simple',title||' '||body));
CREATE INDEX ON crm.connection_evidence USING gin(metadata jsonb_path_ops);
CREATE INDEX ON crm.connection_evidence(dedup_key) WHERE dedup_key IS NOT NULL;
CREATE TABLE crm.connection_evidence_people(evidence_id uuid NOT NULL REFERENCES crm.connection_evidence(id) ON DELETE CASCADE,profile_id uuid NOT NULL REFERENCES crm.profiles(id),role text NOT NULL,PRIMARY KEY(evidence_id,profile_id,role));
CREATE INDEX ON crm.connection_evidence_people(profile_id,evidence_id);
CREATE TABLE crm.connection_relink(evidence_id uuid PRIMARY KEY REFERENCES crm.connection_evidence(id),requested_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE crm.connection_reprocess(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),external_id text NOT NULL,requested_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(connection_id,external_id));
CREATE TABLE crm.mail_rules(connection_id uuid NOT NULL REFERENCES crm.source_connections(id),scope text NOT NULL CHECK(scope IN ('message','thread','sender')),target text NOT NULL,category text NOT NULL CHECK(category IN ('personal','transactional','marketing','pending')),PRIMARY KEY(connection_id,scope,target));

ALTER TABLE crm.field_revisions ADD COLUMN source_record_id text;

INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,state) VALUES('66411572-d779-5177-a186-fb118bc4e441','wechat','legacy-default','wechat legacy account','legacy','legacy');

INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,state) VALUES('c134001e-8ab3-54b3-ace9-46fc3c472680','instagram','legacy-default','instagram legacy account','legacy','legacy');

INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,state) VALUES('07556de6-9c1e-56d6-9a84-85db43cea859','linkedin','legacy-default','linkedin legacy account','legacy','legacy');
