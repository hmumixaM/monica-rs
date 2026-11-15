-- Classify namespace + retained head with a pure expression. Bulk directory
-- reads join the heads once instead of issuing a SQL function lookup per row.
CREATE FUNCTION crm.wechat_is_official(external text, observed_kind text)
RETURNS boolean LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
 SELECT CASE WHEN right(btrim(external),7)='@openim' THEN false
  WHEN left(btrim(external),3)='gh_' THEN true
  ELSE coalesce(observed_kind='official',false) END
$$;
CREATE OR REPLACE FUNCTION crm.wechat_is_official(external text)
RETURNS boolean LANGUAGE sql STABLE PARALLEL SAFE AS $$
 SELECT crm.wechat_is_official(external,(SELECT account_kind
  FROM crm.wechat_directory_accounts WHERE external_id=btrim(external)))
$$;

CREATE OR REPLACE VIEW crm.directory_profiles AS
 WITH classified AS MATERIALIZED (
  SELECT a.profile_id,a.provider,
   a.provider='wechat' AND crm.wechat_is_official(a.external_id,w.account_kind) official
  FROM crm.profile_directory_accounts a LEFT JOIN crm.wechat_directory_accounts w
   ON a.provider='wechat' AND w.external_id=btrim(a.external_id)
 ), hidden AS (
  SELECT profile_id FROM classified GROUP BY profile_id
  HAVING bool_or(official) AND NOT bool_or(provider<>'monica' AND NOT official)
 )
 SELECT p.* FROM crm.profiles p LEFT JOIN hidden h ON h.profile_id=p.id
 WHERE p.archived_at IS NULL AND (p.profile_type<>'person' OR h.profile_id IS NULL
  OR EXISTS(SELECT 1 FROM crm.audit_events e WHERE e.entity_id=p.id AND e.entity_type='profile' AND e.action='profile.created'));

INSERT INTO crm.cache_invalidations
 SELECT DISTINCT pg_current_xact_id(),'profile',a.profile_id FROM crm.profile_directory_accounts a
 WHERE a.provider='wechat' AND crm.wechat_is_official(a.external_id) ORDER BY 3 ON CONFLICT DO NOTHING;
