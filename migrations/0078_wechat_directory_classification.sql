-- A derived head over retained contact evidence. Never archive/delete a person
-- or rewrite their fields to hide a subscription account from the directory.
CREATE TABLE crm.wechat_directory_accounts (
 external_id text PRIMARY KEY,
 account_kind text NOT NULL CHECK(account_kind IN ('person','enterprise','official')),
 verify_flag bigint,
 observed_at timestamptz NOT NULL,
 evidence_key text NOT NULL
);

CREATE FUNCTION crm.wechat_contact_kind(external_id text, payload jsonb)
RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
 SELECT CASE
  -- WeCom customer-service contacts use their own protocol namespace.
  WHEN right(btrim(external_id),7)='@openim' THEN 'enterprise'
  WHEN left(btrim(external_id),3)='gh_' THEN 'official'
  WHEN flag ~ '^[0-9]{1,18}$' THEN
   CASE WHEN (flag::bigint & 8)<>0 THEN 'official' ELSE 'person' END
  ELSE NULL END
 FROM (SELECT coalesce(payload->'raw_contact'->>'verify_flag',
                       payload->>'verify_flag',payload->'fields'->>'verify_flag') flag) f
$$;

CREATE FUNCTION crm.observe_wechat_contact(external text,payload jsonb,observed timestamptz,evidence text)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE kind text; flag text;
BEGIN
 kind=crm.wechat_contact_kind(external,payload);
 IF kind IS NULL OR nullif(btrim(external),'') IS NULL THEN RETURN; END IF;
 flag=coalesce(payload->'raw_contact'->>'verify_flag',payload->>'verify_flag',payload->'fields'->>'verify_flag');
 INSERT INTO crm.wechat_directory_accounts(external_id,account_kind,verify_flag,observed_at,evidence_key)
 VALUES(btrim(external),kind,CASE WHEN flag ~ '^[0-9]{1,18}$' THEN flag::bigint END,observed,evidence)
 ON CONFLICT(external_id) DO UPDATE SET account_kind=excluded.account_kind,
  verify_flag=excluded.verify_flag,observed_at=excluded.observed_at,evidence_key=excluded.evidence_key
 WHERE (excluded.observed_at,excluded.evidence_key)>(crm.wechat_directory_accounts.observed_at,crm.wechat_directory_accounts.evidence_key);
END $$;

CREATE FUNCTION crm.wechat_is_official(external text)
RETURNS boolean LANGUAGE sql STABLE PARALLEL SAFE AS $$
 SELECT CASE WHEN right(btrim(external),7)='@openim' THEN false
  WHEN left(btrim(external),3)='gh_' THEN true
  ELSE coalesce((SELECT account_kind='official' FROM crm.wechat_directory_accounts WHERE external_id=btrim(external)),false) END
$$;

CREATE INDEX directory_manual_creation ON crm.audit_events(entity_id) WHERE entity_type='profile' AND action='profile.created';
CREATE INDEX directory_native_owner ON crm.connection_people(resolved_profile_id,connection_id,external_id) WHERE resolved_profile_id IS NOT NULL;

-- All three attribution paths are supported: migrated source links, notebook
-- identities, and native connector participants. Invalid identities are not a
-- second legitimate source that can accidentally unhide an official account.
CREATE VIEW crm.profile_directory_accounts AS
 SELECT l.profile_id,a.provider,a.external_id
 FROM crm.source_account_links l JOIN crm.source_accounts a ON a.id=l.account_id
 WHERE a.object_kind='person' AND NOT EXISTS(
  SELECT 1 FROM crm.identities i WHERE i.source_account_id=a.id AND i.profile_id=l.profile_id AND i.status IN('invalid','merged'))
 UNION
 SELECT profile_id,provider,external_id FROM crm.identities
 WHERE profile_id IS NOT NULL AND status NOT IN('invalid','merged') AND provider<>'wechat_group'
 UNION
 SELECT p.resolved_profile_id,c.provider,p.external_id
 FROM crm.connection_people p JOIN crm.source_connections c ON c.id=p.connection_id
 WHERE p.resolved_profile_id IS NOT NULL;

CREATE VIEW crm.directory_profiles AS
 SELECT p.* FROM crm.profiles p WHERE p.archived_at IS NULL AND (
  p.profile_type<>'person'
  OR NOT EXISTS(SELECT 1 FROM crm.profile_directory_accounts a WHERE a.profile_id=p.id AND a.provider='wechat' AND crm.wechat_is_official(a.external_id))
  OR EXISTS(SELECT 1 FROM crm.profile_directory_accounts a WHERE a.profile_id=p.id AND NOT(a.provider='wechat' AND crm.wechat_is_official(a.external_id)))
  -- A deliberately created notebook person is not silently converted to a
  -- subscription account because somebody attached a source identity to it.
  OR EXISTS(SELECT 1 FROM crm.audit_events e WHERE e.entity_id=p.id AND e.entity_type='profile' AND e.action='profile.created')
 );

CREATE VIEW crm.directory_relationship_edges AS
 SELECT e.* FROM crm.presented_relationship_edges e
 JOIN crm.directory_profiles a ON a.id=e.from_profile_id
 JOIN crm.directory_profiles b ON b.id=e.to_profile_id;

-- Native contact evidence is already an immutable revision in its source
-- journal. Update this derived head before participant/profile projection.
CREATE FUNCTION crm.classify_connection_contact() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE participant jsonb; observed timestamptz;
BEGIN
 IF NEW.kind='contact' AND EXISTS(SELECT 1 FROM crm.source_connections WHERE id=NEW.connection_id AND provider='wechat') THEN
  SELECT observed_at INTO observed FROM wechat.connection_revision_refs
   WHERE connection_id=NEW.connection_id AND external_id=NEW.external_id AND revision=NEW.revision;
  FOR participant IN SELECT value FROM jsonb_array_elements(CASE WHEN jsonb_typeof(NEW.metadata->'participants')='array' THEN NEW.metadata->'participants' ELSE '[]'::jsonb END) LOOP
   IF nullif(participant->>'id','') IS NOT NULL THEN
    PERFORM crm.observe_wechat_contact(participant->>'id',NEW.metadata,coalesce(observed,now()),
     'connection:'||NEW.connection_id||':'||NEW.external_id||':'||lpad(NEW.revision::text,20,'0'));
   END IF;
  END LOOP;
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER classify_contact AFTER INSERT OR UPDATE ON crm.connection_evidence
 FOR EACH ROW WHEN (NEW.kind='contact') EXECUTE FUNCTION crm.classify_connection_contact();

CREATE FUNCTION crm.classify_chatlog_contact() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.file_name='contact/contact.db' AND NEW.table_name='contact' THEN
  PERFORM crm.observe_wechat_contact(NEW.payload->>'username',NEW.payload,
   (SELECT created_at FROM crm.legacy_chatlog_snapshots WHERE id=NEW.snapshot_id),
   'chatlog:'||NEW.snapshot_id||':'||NEW.row_key);
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER classify_contact AFTER INSERT ON crm.legacy_chatlog_history
 FOR EACH ROW WHEN (NEW.file_name='contact/contact.db' AND NEW.table_name='contact') EXECUTE FUNCTION crm.classify_chatlog_contact();

-- Existing migration snapshots retain every raw contact row, including contacts
-- excluded by the old prefix-only reader. Rebuild from those local receipts.
DO $$DECLARE r record; BEGIN
 FOR r IN SELECT a.external_id,f.value,f.observed_at,f.object_id,f.source_version
  FROM crm.source_fields f JOIN crm.source_accounts a ON a.id=f.account_id
  WHERE a.provider='wechat' AND f.field_key IN('wechat.verify_flag','verify_flag')
 LOOP
  PERFORM crm.observe_wechat_contact(r.external_id,jsonb_build_object('verify_flag',r.value),r.observed_at,
   'object:'||r.object_id||':'||lpad(r.source_version::text,20,'0'));
 END LOOP;
 FOR r IN SELECT h.payload,s.created_at,h.snapshot_id,h.row_key
  FROM crm.legacy_chatlog_history h JOIN crm.legacy_chatlog_snapshots s ON s.id=h.snapshot_id
  WHERE h.file_name='contact/contact.db' AND h.table_name='contact'
  ORDER BY s.created_at,h.snapshot_id,h.row_key
 LOOP
  PERFORM crm.observe_wechat_contact(r.payload->>'username',r.payload,r.created_at,'chatlog:'||r.snapshot_id||':'||r.row_key);
 END LOOP;
END $$;

CREATE TRIGGER cache_i AFTER INSERT ON crm.wechat_directory_accounts REFERENCING NEW TABLE AS added FOR EACH STATEMENT
 EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN changed c ON c.external_id=a.external_id WHERE a.provider=''wechat'' UNION ALL SELECT ''identity'',a.id FROM crm.source_accounts a JOIN changed c ON c.external_id=a.external_id WHERE a.provider=''wechat''');
CREATE TRIGGER cache_u AFTER UPDATE ON crm.wechat_directory_accounts REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT
 EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN (SELECT external_id,account_kind FROM added EXCEPT SELECT external_id,account_kind FROM removed)c ON c.external_id=a.external_id WHERE a.provider=''wechat'' UNION ALL SELECT ''identity'',a.id FROM crm.source_accounts a JOIN (SELECT external_id,account_kind FROM added EXCEPT SELECT external_id,account_kind FROM removed)c ON c.external_id=a.external_id WHERE a.provider=''wechat''');
-- A backfill may hide pre-existing contacts before the triggers were installed.
INSERT INTO crm.cache_invalidations
 SELECT DISTINCT pg_current_xact_id(),'profile',a.profile_id FROM crm.profile_directory_accounts a
 WHERE a.provider='wechat' AND crm.wechat_is_official(a.external_id) ORDER BY 3 ON CONFLICT DO NOTHING;
