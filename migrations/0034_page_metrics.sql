-- Metrics are durable samples produced by the bounded worker, never fabricated
-- by a GET. No samples exist until a source is present and refresh runs.
CREATE TABLE crm.page_metric_snapshots (
 scope text NOT NULL CHECK(scope IN('global','profile')),subject uuid NOT NULL,metric_key text NOT NULL,
 value_json jsonb NOT NULL,source_watermark text NOT NULL,computed_at timestamptz NOT NULL DEFAULT now(),
 calculation_version integer NOT NULL DEFAULT 1,freshness_state text NOT NULL DEFAULT 'fresh',
 PRIMARY KEY(scope,subject,metric_key)
);
CREATE FUNCTION crm.refresh_page_metrics() RETURNS boolean LANGUAGE plpgsql AS $$
DECLARE watermark text;previous text;changed bigint=0;affected bigint=0;empty uuid='00000000-0000-0000-0000-000000000000';
BEGIN
 IF NOT pg_try_advisory_xact_lock(hashtextextended('crm.page-metrics-refresh',0)) OR NOT EXISTS(SELECT 1 FROM crm.source_accounts) THEN RETURN false;END IF;
 SELECT md5(coalesce(string_agg(domain||subject::text||version::text,',' ORDER BY domain,subject),'')) INTO watermark FROM crm.cache_versions WHERE domain IN('profile','posts','identity','activity','metrics');
 SELECT source_watermark INTO previous FROM crm.page_metric_snapshots WHERE scope='global' AND subject=empty AND metric_key='global.profile_count';
 IF previous IS DISTINCT FROM watermark THEN
  INSERT INTO crm.page_metric_snapshots(scope,subject,metric_key,value_json,source_watermark)
  SELECT 'global',empty,key,to_jsonb(value),watermark FROM (
   SELECT 'global.profile_count' key,count(*) value FROM crm.profiles WHERE archived_at IS NULL
   UNION ALL SELECT 'global.timeline_count',(SELECT count(*) FROM crm.page_posts)+(SELECT count(*) FROM crm.activities WHERE activity_type NOT IN('wechat_direct_summary','wechat_group_summary'))
   UNION ALL SELECT 'provider.'||provider||'.coverage',(SELECT count(*) FROM crm.page_entity_counts WHERE metric=provider||'.observations' AND value>0) FROM unnest(ARRAY['wechat','instagram','linkedin']) provider
  ) samples ON CONFLICT(scope,subject,metric_key) DO UPDATE SET value_json=excluded.value_json,source_watermark=excluded.source_watermark,computed_at=now(),freshness_state='fresh';
  GET DIAGNOSTICS changed=ROW_COUNT;
 END IF;
 -- At most 128 dirty profile subjects per invocation. Independent changes do
 -- not rewrite every profile sample or acquire their rows.
 WITH candidates AS (
  SELECT p.id,f.version FROM crm.profiles p CROSS JOIN LATERAL(
   SELECT md5(coalesce(string_agg(v.domain||v.version::text,',' ORDER BY v.domain),'')) version FROM crm.cache_versions v WHERE v.subject=p.id AND v.domain IN('profile','posts','profile-activity')
  ) f WHERE p.archived_at IS NULL AND NOT EXISTS(SELECT 1 FROM crm.page_metric_snapshots m WHERE m.scope='profile' AND m.subject=p.id AND m.metric_key='timeline.count' AND m.source_watermark=f.version)
  ORDER BY p.id LIMIT 128
 ), samples AS (
  SELECT c.id,c.version,'timeline.count' key,(SELECT count(*) FROM crm.page_posts WHERE profile_id=c.id)+(SELECT count(*) FROM crm.activity_participants ap JOIN crm.activities a ON a.id=ap.activity_id WHERE ap.profile_id=c.id AND a.activity_type NOT IN('wechat_direct_summary','wechat_group_summary')) value FROM candidates c
  UNION ALL SELECT c.id,c.version,'identity.count',(SELECT count(*) FROM crm.identities WHERE profile_id=c.id AND status='active') FROM candidates c
 ) INSERT INTO crm.page_metric_snapshots(scope,subject,metric_key,value_json,source_watermark)
 SELECT 'profile',id,key,to_jsonb(value),version FROM samples
 ON CONFLICT(scope,subject,metric_key) DO UPDATE SET value_json=excluded.value_json,source_watermark=excluded.source_watermark,computed_at=now(),freshness_state='fresh';
 GET DIAGNOSTICS affected=ROW_COUNT;
 RETURN changed+affected>0;
END $$;
CREATE TRIGGER cache_i AFTER INSERT ON crm.page_metric_snapshots REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''metric-snapshot'' domain,subject FROM changed');
CREATE TRIGGER cache_u AFTER UPDATE ON crm.page_metric_snapshots REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''metric-snapshot'' domain,subject FROM changed');
CREATE TRIGGER cache_d AFTER DELETE ON crm.page_metric_snapshots REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''metric-snapshot'' domain,subject FROM changed');
CREATE TRIGGER cache_i AFTER INSERT ON crm.activity_participants REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile-activity'' domain,profile_id subject FROM changed');
CREATE TRIGGER cache_u AFTER UPDATE ON crm.activity_participants REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile-activity'' domain,profile_id subject FROM changed');
CREATE TRIGGER cache_d AFTER DELETE ON crm.activity_participants REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''profile-activity'' domain,profile_id subject FROM changed');
