-- Match the predicates actually used by uncached person/media/feed reads.
CREATE INDEX identities_profile_current ON crm.identities(profile_id,provider,username,id)
 WHERE status NOT IN ('invalid','merged');
CREATE INDEX media_links_entity ON crm.media_links(entity_id,role,media_id);
CREATE INDEX source_group_memberships_person_current ON crm.source_group_memberships(person_account_id,group_account_id)
 WHERE left_at IS NULL;
CREATE INDEX conversations_recent ON crm.conversations(last_message_at DESC NULLS LAST,id DESC);
CREATE INDEX conversations_profile_recent ON crm.conversations(profile_id,last_message_at DESC NULLS LAST,id DESC);
CREATE INDEX timeline_current_recent ON crm.timeline_events(occurred_at DESC NULLS LAST,id DESC)
 WHERE status<>'deleted';
CREATE INDEX timeline_profile_current_recent ON crm.timeline_events(profile_id,occurred_at DESC NULLS LAST,id DESC)
 WHERE status<>'deleted';
CREATE INDEX page_posts_profile_recent ON crm.page_posts(profile_id,occurred_at DESC NULLS LAST,id DESC)
 WHERE kind='post';

-- Global page validation reads at most32 epochs per domain, not every person's
-- token. Partition writers by transaction; never serialize all imports on one row.
CREATE TABLE crm.cache_domain_versions(
 domain text NOT NULL,shard smallint NOT NULL CHECK(shard BETWEEN 0 AND 31),
 version uuid NOT NULL,PRIMARY KEY(domain,shard)
);
INSERT INTO crm.cache_domain_versions
 SELECT domain,0,gen_random_uuid() FROM crm.cache_versions GROUP BY domain ORDER BY domain;
CREATE OR REPLACE FUNCTION crm.flush_cache_invalidations() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
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
 DELETE FROM crm.cache_flush_requests WHERE transaction_id=NEW.transaction_id;
 RETURN NULL;
END $$;

-- Periodic aggregation uses the same bounded global dependency epochs.
CREATE OR REPLACE FUNCTION crm.refresh_page_metrics() RETURNS boolean LANGUAGE plpgsql AS $$
DECLARE watermark text;previous text;changed bigint=0;affected bigint=0;empty uuid='00000000-0000-0000-0000-000000000000';
BEGIN
 IF NOT pg_try_advisory_xact_lock(hashtextextended('crm.page-metrics-refresh',0)) OR NOT EXISTS(SELECT 1 FROM crm.source_accounts) THEN RETURN false;END IF;
 SELECT md5(coalesce(string_agg(domain||':'||shard::text||':'||version::text,',' ORDER BY domain,shard),'')) INTO watermark FROM crm.cache_domain_versions WHERE domain IN('profile','posts','identity','activity','metrics');
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
