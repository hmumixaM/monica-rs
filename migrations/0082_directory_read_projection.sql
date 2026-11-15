-- Keep directory eligibility in a small transactional read projection. Source
-- facts remain untouched. The existing sorted cache-version upserts serialize
-- overlapping profile changes before the projection is refreshed.
CREATE TABLE crm.directory_exclusions (
 profile_id uuid PRIMARY KEY REFERENCES crm.profiles(id) ON DELETE CASCADE
);

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

CREATE FUNCTION crm.refresh_directory_exclusions(owners uuid[]) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
 IF coalesce(cardinality(owners),0)=0 THEN RETURN; END IF;
 WITH desired AS MATERIALIZED (
  SELECT p.id FROM crm.profiles p WHERE p.id=ANY(owners) AND crm.directory_profile_excluded(p.id)
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
  SELECT subject FROM crm.cache_invalidations
  WHERE transaction_id=pg_current_xact_id_if_assigned() AND domain='profile'
 )
 SELECT p.* FROM crm.profiles p
 LEFT JOIN crm.directory_exclusions e ON e.profile_id=p.id
 LEFT JOIN pending changed ON changed.subject=p.id
 WHERE p.archived_at IS NULL AND CASE WHEN changed.subject IS NULL THEN e.profile_id IS NULL
  ELSE NOT crm.directory_profile_excluded(p.id) END;

-- Most producers already invalidate profiles. Cover the remaining directory
-- inputs without invalidating on routine display/heartbeat/settings updates.
CREATE TRIGGER directory_policy_i AFTER INSERT ON crm.audit_events REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,entity_id subject FROM changed WHERE entity_type=''profile'' AND action=''profile.created''');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.audit_events REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,entity_id subject FROM changed WHERE entity_type=''profile'' AND action=''profile.created''');
CREATE TRIGGER directory_policy_d AFTER DELETE ON crm.audit_events REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,entity_id subject FROM changed WHERE entity_type=''profile'' AND action=''profile.created''');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.source_accounts REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,profile_id subject FROM (WITH affected AS ((SELECT id,provider,external_id,object_kind FROM added EXCEPT SELECT id,provider,external_id,object_kind FROM removed) UNION ALL (SELECT id,provider,external_id,object_kind FROM removed EXCEPT SELECT id,provider,external_id,object_kind FROM added)) SELECT l.profile_id FROM affected c JOIN crm.source_account_links l ON l.account_id=c.id) owners(profile_id)');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.source_connections REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,profile_id subject FROM (WITH affected AS ((SELECT id,provider FROM added EXCEPT SELECT id,provider FROM removed) UNION ALL (SELECT id,provider FROM removed EXCEPT SELECT id,provider FROM added)) SELECT p.resolved_profile_id FROM affected c JOIN crm.connection_people p ON p.connection_id=c.id) owners(profile_id)');
CREATE TRIGGER directory_policy_i AFTER INSERT ON crm.wechat_directory_accounts REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN (SELECT external_id FROM changed WHERE crm.wechat_is_official(external_id,account_kind) IS DISTINCT FROM crm.wechat_is_official(external_id,NULL)) c ON c.external_id=btrim(a.external_id) WHERE a.provider=''wechat''');
CREATE TRIGGER directory_policy_u AFTER UPDATE ON crm.wechat_directory_accounts REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN ((SELECT external_id,crm.wechat_is_official(external_id,account_kind) official FROM added EXCEPT SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM removed) UNION ALL (SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM removed EXCEPT SELECT external_id,crm.wechat_is_official(external_id,account_kind) FROM added)) c ON c.external_id=btrim(a.external_id) WHERE a.provider=''wechat''');
CREATE TRIGGER directory_policy_d AFTER DELETE ON crm.wechat_directory_accounts REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile'' domain,a.profile_id subject FROM crm.profile_directory_accounts a JOIN (SELECT external_id FROM changed WHERE crm.wechat_is_official(external_id,account_kind) IS DISTINCT FROM crm.wechat_is_official(external_id,NULL)) c ON c.external_id=btrim(a.external_id) WHERE a.provider=''wechat''');
