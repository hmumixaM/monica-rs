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

-- Share attribution payloads and privacy eligibility without repeating a
-- population join for every object selected by a deferred batch.
CREATE VIEW crm.memory_legacy_objects AS
 SELECT e.* FROM (
 SELECT 'message'::text kind,m.message_id entity_id,
 CASE WHEN c.conversation_type='group' THEN coalesce(m.sender_profile_id,l.profile_id) ELSE c.profile_id END profile_id,
 'chat'::text source_kind,true private,
 jsonb_build_object('id',m.message_id,'kind',c.conversation_type,'provider',m.provider,'at',m.occurred_at,'conversation_id',m.conversation_id,'text',m.body,'sender',m.sender,'sender_profile_id',coalesce(m.sender_profile_id,l.profile_id),'direction',coalesce(m.direction,CASE WHEN m.sender=m.account THEN 'sent' ELSE 'received' END),'speaker',CASE WHEN c.conversation_type='group' THEN 'TA' WHEN m.direction='unknown' THEN '未知' WHEN m.direction='sent' OR (m.direction IS NULL AND m.sender=m.account) THEN '我' ELSE 'TA' END,'group',CASE WHEN c.conversation_type='group' THEN coalesce(c.source_display_name,c.external_id) END) payload
 FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id
 LEFT JOIN crm.source_account_links l ON l.account_id=m.sender_account_id
 WHERE c.connection_id IS NULL AND NOT m.tombstone
 UNION ALL
 SELECT 'social',e.id,e.profile_id,'social',false,jsonb_build_object('id',e.id,'kind',e.event_type,'provider',e.provider,'at',e.occurred_at,'content',e.content)
 FROM crm.timeline_events e WHERE e.status<>'deleted' AND NOT EXISTS(SELECT 1 FROM crm.connection_evidence source WHERE source.id=e.id)
 UNION ALL
 SELECT 'activity',a.id,ap.profile_id,'note',true,jsonb_build_object('id',a.id,'kind',a.activity_type,'at',a.occurred_at,'title',a.title,'text',a.body,'state',a.state,'due_at',a.due_at)
 FROM crm.activities a JOIN crm.activity_participants ap ON ap.activity_id=a.id
 WHERE a.activity_type NOT IN ('wechat_direct_summary','wechat_group_summary')
 ) e;
CREATE VIEW crm.memory_legacy_eligible_profiles AS
 SELECT p.id FROM crm.profiles p WHERE p.profile_type='person' AND p.archived_at IS NULL
 AND NOT EXISTS(SELECT 1 FROM crm.person_identifiers i WHERE i.profile_id=p.id AND i.is_self);
CREATE OR REPLACE VIEW crm.memory_legacy_current AS
 SELECT e.* FROM crm.memory_legacy_objects e JOIN crm.memory_legacy_eligible_profiles p ON p.id=e.profile_id;
CREATE FUNCTION crm.memory_legacy_selected(record_kind text,record_ids uuid[])
 RETURNS SETOF crm.memory_legacy_current LANGUAGE sql STABLE AS $$
 WITH selected AS MATERIALIZED(
  SELECT * FROM crm.memory_legacy_objects WHERE kind=record_kind AND entity_id=ANY(record_ids)
 ) SELECT e.* FROM selected e JOIN crm.memory_legacy_eligible_profiles p ON p.id=e.profile_id
$$;

-- Deferred reductions see the entire completed import. Keep object lookups
-- bounded before the shared profile-eligibility join.
CREATE OR REPLACE FUNCTION crm.memory_sync_legacy(record_kind text,record_ids uuid[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE owners uuid[];
BEGIN
 IF cardinality(record_ids)>1000 THEN RAISE EXCEPTION 'legacy Memory batch exceeds 1000'; END IF;
 SELECT array_agg(profile_id ORDER BY profile_id) INTO owners FROM (
 SELECT profile_id FROM crm.memory_legacy_owners WHERE kind=record_kind AND entity_id=ANY(record_ids)
 UNION SELECT profile_id FROM crm.memory_legacy_selected(record_kind,record_ids)) p;
 IF owners IS NULL THEN RETURN; END IF;
 INSERT INTO crm.memory_heads(profile_id) SELECT unnest(owners) ON CONFLICT DO NOTHING;
 PERFORM profile_id FROM crm.memory_heads WHERE profile_id=ANY(owners) ORDER BY profile_id FOR UPDATE;
 WITH wanted AS MATERIALIZED (
 SELECT c.*,encode(sha256(convert_to('legacy:'||kind||':'||entity_id::text,'UTF8')),'hex') source_key,
 'legacy:'||kind||':'||entity_id::text source_id FROM crm.memory_legacy_selected(record_kind,record_ids) c
 ), inserted AS (
 INSERT INTO crm.memory_inputs(profile_id,source_key,source_kind,source_id,bucket,payload,input_hash,private,payload_bytes)
 SELECT profile_id,source_key,source_kind,source_id,left(source_key,2),jsonb_build_object('_legacy_kind',kind,'_legacy_id',entity_id),encode(sha256(convert_to(payload::text,'UTF8')),'hex'),private,octet_length(payload::text) FROM wanted ORDER BY profile_id,source_key
 ON CONFLICT(profile_id,source_key) DO UPDATE SET payload=excluded.payload,input_hash=excluded.input_hash,private=excluded.private,payload_bytes=excluded.payload_bytes
 WHERE memory_inputs.input_hash<>excluded.input_hash OR memory_inputs.private<>excluded.private OR memory_inputs.payload<>excluded.payload RETURNING profile_id
 ), removed AS (
 DELETE FROM crm.memory_inputs m USING crm.memory_legacy_owners o WHERE o.kind=record_kind AND o.entity_id=ANY(record_ids) AND m.profile_id=o.profile_id AND m.source_id='legacy:'||o.kind||':'||o.entity_id::text
 AND NOT EXISTS(SELECT 1 FROM wanted w WHERE w.profile_id=o.profile_id AND w.entity_id=o.entity_id) RETURNING m.profile_id
 ), saved AS (
 INSERT INTO crm.memory_legacy_owners SELECT kind,entity_id,profile_id FROM wanted ON CONFLICT DO NOTHING RETURNING profile_id
 ), forgotten AS (
 DELETE FROM crm.memory_legacy_owners o WHERE o.kind=record_kind AND o.entity_id=ANY(record_ids) AND NOT EXISTS(SELECT 1 FROM wanted w WHERE w.profile_id=o.profile_id AND w.entity_id=o.entity_id) RETURNING profile_id
 ) UPDATE crm.memory_heads SET generation=generation+1,updated_at=now() WHERE profile_id IN(SELECT profile_id FROM inserted UNION SELECT profile_id FROM removed);
END $$;
