SET LOCAL lock_timeout='3s';
LOCK TABLE crm.profiles,crm.identities,crm.source_account_links,crm.connection_people,
 crm.source_accounts,crm.source_connections,crm.wechat_directory_accounts,crm.audit_events
 IN SHARE ROW EXCLUSIVE MODE;
-- Keep directory eligibility in a small transactional read projection. Source
-- facts remain untouched. The existing sorted cache-version upserts serialize
-- overlapping profile changes before the projection is refreshed.
CREATE TABLE crm.directory_exclusions (
 profile_id uuid PRIMARY KEY
);

-- Dependency epochs cover parent metadata that can change while its new owner
-- is still invisible to another transaction. These are derived metadata rows;
-- deletion is handled by the existing profile publication, without source FKs.
CREATE TABLE crm.directory_policy_dependencies (
 profile_id uuid NOT NULL,source_key uuid NOT NULL,version uuid,
 PRIMARY KEY(profile_id,source_key)
);
CREATE FUNCTION crm.directory_dependency_key(kind text,value text) RETURNS uuid
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$ SELECT md5(kind||':'||value)::uuid $$;
CREATE FUNCTION crm.directory_dependency_sources(owners uuid[])
RETURNS TABLE(profile_id uuid,source_key uuid) LANGUAGE sql STABLE AS $$
 SELECT a.profile_id,crm.directory_dependency_key('wechat',btrim(a.external_id))
 FROM crm.profile_directory_accounts a WHERE a.profile_id=ANY(owners) AND a.provider='wechat'
 UNION SELECT l.profile_id,crm.directory_dependency_key('account',l.account_id::text)
 FROM crm.source_account_links l WHERE l.profile_id=ANY(owners)
 UNION SELECT p.resolved_profile_id,crm.directory_dependency_key('connection',p.connection_id::text)
 FROM crm.connection_people p WHERE p.resolved_profile_id=ANY(owners)
$$;

CREATE FUNCTION crm.directory_profile_excluded(owner_id uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
 SELECT coalesce(bool_or(a.provider='wechat' AND crm.wechat_is_official(a.external_id))
  AND NOT bool_or(a.provider<>'monica' AND NOT(a.provider='wechat' AND crm.wechat_is_official(a.external_id))),false)
  AND EXISTS(SELECT 1 FROM crm.profiles p WHERE p.id=owner_id AND p.profile_type='person' AND p.archived_at IS NULL)
  AND NOT EXISTS(SELECT 1 FROM crm.audit_events e WHERE e.entity_id=owner_id AND e.entity_type='profile' AND e.action='profile.created')
 FROM crm.profile_directory_accounts a WHERE a.profile_id=owner_id
$$;

-- Read the old view before replacing it. Only derived exclusions are inserted;
-- no profile, identity, source record or existing decision is rewritten.
INSERT INTO crm.directory_exclusions(profile_id)
 SELECT p.id FROM crm.profiles p WHERE p.archived_at IS NULL AND p.profile_type='person'
 AND NOT EXISTS(SELECT 1 FROM crm.directory_profiles visible WHERE visible.id=p.id);

INSERT INTO crm.directory_policy_dependencies(profile_id,source_key,version)
 SELECT d.profile_id,d.source_key,v.version FROM crm.directory_dependency_sources(
  ARRAY(SELECT id FROM crm.profiles WHERE archived_at IS NULL AND profile_type='person')) d
 LEFT JOIN crm.cache_versions v ON v.domain='directory' AND v.subject=d.source_key;

CREATE FUNCTION crm.refresh_directory_exclusions(owners uuid[]) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
 IF coalesce(cardinality(owners),0)=0 THEN RETURN; END IF;
 -- Eligibility and dependency versions use the SAME statement snapshot.
 WITH owners_now AS MATERIALIZED (
  SELECT p.id,crm.directory_profile_excluded(p.id) excluded FROM crm.profiles p
  WHERE p.id=ANY(owners) AND p.archived_at IS NULL AND p.profile_type='person'
 ), desired AS MATERIALIZED (SELECT id FROM owners_now WHERE excluded),
 dependencies_now AS MATERIALIZED (
  SELECT d.profile_id,d.source_key,v.version FROM crm.directory_dependency_sources(
   ARRAY(SELECT id FROM owners_now)) d
  LEFT JOIN crm.cache_versions v ON v.domain='directory' AND v.subject=d.source_key
 ), removed_dependencies AS (
  DELETE FROM crm.directory_policy_dependencies old WHERE old.profile_id=ANY(owners)
   AND NOT EXISTS(SELECT 1 FROM dependencies_now d WHERE (d.profile_id,d.source_key)=(old.profile_id,old.source_key))
 ), saved_dependencies AS (
  INSERT INTO crm.directory_policy_dependencies(profile_id,source_key,version)
  SELECT profile_id,source_key,version FROM dependencies_now ORDER BY profile_id,source_key
  ON CONFLICT(profile_id,source_key) DO UPDATE SET version=excluded.version
  WHERE crm.directory_policy_dependencies.version IS DISTINCT FROM excluded.version
 ), removed AS (
  DELETE FROM crm.directory_exclusions e WHERE e.profile_id=ANY(owners)
   AND NOT EXISTS(SELECT 1 FROM desired d WHERE d.id=e.profile_id)
 )
 INSERT INTO crm.directory_exclusions(profile_id) SELECT id FROM desired ORDER BY id ON CONFLICT DO NOTHING;
END $$;

CREATE OR REPLACE FUNCTION crm.flush_cache_invalidations() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE directory_owners uuid[];
BEGIN
 SELECT array_agg(subject ORDER BY subject) INTO directory_owners FROM crm.cache_invalidations
 WHERE transaction_id=NEW.transaction_id AND domain='profile';
 WITH pending AS(DELETE FROM crm.cache_invalidations WHERE transaction_id=NEW.transaction_id RETURNING domain,subject),
 saved AS(
  INSERT INTO crm.cache_versions(domain,subject,version)
  SELECT domain,subject,gen_random_uuid() FROM pending ORDER BY domain,subject
  ON CONFLICT(domain,subject) DO UPDATE SET version=excluded.version RETURNING domain,subject
 )
 INSERT INTO crm.cache_domain_versions(domain,shard,version)
 SELECT domain,shard,gen_random_uuid() FROM(
  SELECT DISTINCT domain,(NEW.transaction_id::text::numeric%32)::smallint shard FROM saved
 ) s ORDER BY domain,shard
 ON CONFLICT(domain,shard) DO UPDATE SET version=excluded.version;
 -- The preceding upserts already hold the existing profile metadata locks.
 -- This separate statement sees commits that completed while those locks waited.
 PERFORM crm.refresh_directory_exclusions(directory_owners);
 DELETE FROM crm.cache_flush_requests WHERE transaction_id=NEW.transaction_id;
 RETURN NULL;
END $$;

-- Writers can read their own eligibility changes before deferred publication.
-- Normal read-only requests have no pending profile subjects and use the head.
CREATE OR REPLACE VIEW crm.directory_profiles AS
 WITH pending AS MATERIALIZED (
  SELECT domain,subject FROM crm.cache_invalidations
  WHERE transaction_id=pg_current_xact_id_if_assigned() AND domain IN('profile','directory')
 ), stale AS MATERIALIZED (
  SELECT DISTINCT d.profile_id FROM crm.directory_policy_dependencies d
  LEFT JOIN crm.cache_versions v ON v.domain='directory' AND v.subject=d.source_key
  WHERE d.version IS DISTINCT FROM v.version
   OR EXISTS(SELECT 1 FROM pending p WHERE p.domain='directory' AND p.subject=d.source_key)
 )
 SELECT p.* FROM crm.profiles p
 LEFT JOIN crm.directory_exclusions e ON e.profile_id=p.id
 LEFT JOIN pending changed ON changed.domain='profile' AND changed.subject=p.id
 LEFT JOIN stale outdated ON outdated.profile_id=p.id
 WHERE p.archived_at IS NULL AND CASE WHEN changed.subject IS NULL AND outdated.profile_id IS NULL THEN e.profile_id IS NULL
  ELSE NOT crm.directory_profile_excluded(p.id) END;

-- Most producers already invalidate profiles. Cover the remaining directory
-- inputs without invalidating on routine display/heartbeat/settings updates.
CREATE TRIGGER directory_policy_i AFTER INSERT ON crm.audit_events REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,entity_id subject FROM changed WHERE entity_type=''profile'' AND action=''profile.created''');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.audit_events REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,entity_id subject FROM changed WHERE entity_type=''profile'' AND action=''profile.created''');
CREATE TRIGGER directory_policy_d AFTER DELETE ON crm.audit_events REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,entity_id subject FROM changed WHERE entity_type=''profile'' AND action=''profile.created''');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.source_accounts REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,profile_id subject FROM (WITH affected AS ((SELECT id,provider,external_id,object_kind FROM added EXCEPT SELECT id,provider,external_id,object_kind FROM removed) UNION ALL (SELECT id,provider,external_id,object_kind FROM removed EXCEPT SELECT id,provider,external_id,object_kind FROM added)) SELECT l.profile_id FROM affected c JOIN crm.source_account_links l ON l.account_id=c.id) owners(profile_id) UNION ALL SELECT ''directory'',crm.directory_dependency_key(''account'',id::text) FROM ((SELECT id,provider,external_id,object_kind FROM added EXCEPT SELECT id,provider,external_id,object_kind FROM removed) UNION ALL (SELECT id,provider,external_id,object_kind FROM removed EXCEPT SELECT id,provider,external_id,object_kind FROM added)) changed_accounts');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.source_connections REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,profile_id subject FROM (WITH affected AS ((SELECT id,provider FROM added EXCEPT SELECT id,provider FROM removed) UNION ALL (SELECT id,provider FROM removed EXCEPT SELECT id,provider FROM added)) SELECT p.resolved_profile_id FROM affected c JOIN crm.connection_people p ON p.connection_id=c.id) owners(profile_id) UNION ALL SELECT ''directory'',crm.directory_dependency_key(''connection'',id::text) FROM ((SELECT id,provider FROM added EXCEPT SELECT id,provider FROM removed) UNION ALL (SELECT id,provider FROM removed EXCEPT SELECT id,provider FROM added)) changed_connections');
CREATE TRIGGER directory_policy_i AFTER INSERT ON crm.wechat_directory_accounts REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN (SELECT external_id FROM changed WHERE crm.wechat_is_official(external_id,account_kind) IS DISTINCT FROM crm.wechat_is_official(external_id,NULL)) c ON c.external_id=btrim(a.external_id) WHERE a.provider=''wechat'' UNION ALL SELECT ''directory'',crm.directory_dependency_key(''wechat'',btrim(external_id)) FROM (SELECT external_id FROM changed WHERE crm.wechat_is_official(external_id,account_kind) IS DISTINCT FROM crm.wechat_is_official(external_id,NULL)) changed_heads');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.wechat_directory_accounts REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN ((SELECT external_id,crm.wechat_is_official(external_id,account_kind) official FROM added EXCEPT SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM removed) UNION ALL (SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM removed EXCEPT SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM added)) c ON c.external_id=btrim(a.external_id) WHERE a.provider=''wechat'' UNION ALL SELECT ''directory'',crm.directory_dependency_key(''wechat'',btrim(external_id)) FROM ((SELECT external_id,crm.wechat_is_official(external_id,account_kind) official FROM added EXCEPT SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM removed) UNION ALL (SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM removed EXCEPT SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM added)) changed_heads');
CREATE TRIGGER directory_policy_d AFTER DELETE ON crm.wechat_directory_accounts REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN (SELECT external_id FROM changed WHERE crm.wechat_is_official(external_id,account_kind) IS DISTINCT FROM crm.wechat_is_official(external_id,NULL)) c ON c.external_id=btrim(a.external_id) WHERE a.provider=''wechat'' UNION ALL SELECT ''directory'',crm.directory_dependency_key(''wechat'',btrim(external_id)) FROM (SELECT external_id FROM changed WHERE crm.wechat_is_official(external_id,account_kind) IS DISTINCT FROM crm.wechat_is_official(external_id,NULL)) changed_heads');
