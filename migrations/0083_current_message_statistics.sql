-- Current message counts are separate from immutable source snapshots. Each
-- statement aggregates signed deltas before touching conversation-scoped heads.
CREATE FUNCTION crm.message_stat_kind(direction text,sender text,account text,message_type text)
RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
 SELECT CASE WHEN message_type IN ('10000','10002','system') THEN 'system'
 WHEN direction IN ('sent','received','unknown') THEN direction
 WHEN sender=account THEN 'sent' ELSE 'received' END
$$;
CREATE TABLE crm.message_stat_counts (
 conversation_id uuid NOT NULL REFERENCES crm.conversations(id) ON DELETE CASCADE,
 scope text NOT NULL CHECK(scope IN ('all','month:UTC','month:America/Chicago','day:America/Chicago')),
 period text NOT NULL,
 sent bigint NOT NULL DEFAULT 0,received bigint NOT NULL DEFAULT 0,
 system bigint NOT NULL DEFAULT 0,unknown bigint NOT NULL DEFAULT 0,
 PRIMARY KEY(conversation_id,scope,period)
);
CREATE INDEX message_stat_period ON crm.message_stat_counts(scope,period,conversation_id);
INSERT INTO crm.message_stat_counts
 SELECT m.conversation_id,b.scope,b.period,
 count(*) FILTER(WHERE k.kind='sent'),count(*) FILTER(WHERE k.kind='received'),
 count(*) FILTER(WHERE k.kind='system'),count(*) FILTER(WHERE k.kind='unknown')
 FROM crm.message_search m
 CROSS JOIN LATERAL (SELECT crm.message_stat_kind(m.direction,m.sender,m.account,m.message_type) kind) k
 CROSS JOIN LATERAL (VALUES
 ('all',''),('month:UTC',to_char(m.occurred_at AT TIME ZONE 'UTC','YYYY-MM')),
 ('month:America/Chicago',to_char(m.occurred_at AT TIME ZONE 'America/Chicago','YYYY-MM')),
 ('day:America/Chicago',to_char(m.occurred_at AT TIME ZONE 'America/Chicago','YYYY-MM-DD'))) b(scope,period)
 WHERE NOT m.tombstone GROUP BY m.conversation_id,b.scope,b.period;
CREATE FUNCTION crm.project_message_stat_counts() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;
BEGIN
 source_sql := CASE TG_OP
 WHEN 'INSERT' THEN 'SELECT *,1::bigint delta FROM added WHERE NOT tombstone'
 WHEN 'DELETE' THEN 'SELECT *,-1::bigint delta FROM removed WHERE NOT tombstone'
 ELSE 'SELECT *,1::bigint delta FROM added WHERE NOT tombstone UNION ALL SELECT *,-1::bigint delta FROM removed WHERE NOT tombstone' END;
 EXECUTE 'INSERT INTO crm.message_stat_counts(conversation_id,scope,period,sent,received,system,unknown)
 SELECT d.conversation_id,b.scope,b.period,
 coalesce(sum(delta) FILTER(WHERE k.kind=''sent''),0),coalesce(sum(delta) FILTER(WHERE k.kind=''received''),0),
 coalesce(sum(delta) FILTER(WHERE k.kind=''system''),0),coalesce(sum(delta) FILTER(WHERE k.kind=''unknown''),0)
 FROM ('||source_sql||') d
 CROSS JOIN LATERAL (SELECT crm.message_stat_kind(d.direction,d.sender,d.account,d.message_type) kind) k
 CROSS JOIN LATERAL (VALUES (''all'',''''),(''month:UTC'',to_char(d.occurred_at AT TIME ZONE ''UTC'',''YYYY-MM'')),
 (''month:America/Chicago'',to_char(d.occurred_at AT TIME ZONE ''America/Chicago'',''YYYY-MM'')),
 (''day:America/Chicago'',to_char(d.occurred_at AT TIME ZONE ''America/Chicago'',''YYYY-MM-DD''))) b(scope,period)
 GROUP BY d.conversation_id,b.scope,b.period
 HAVING coalesce(sum(delta) FILTER(WHERE k.kind=''sent''),0)<>0 OR coalesce(sum(delta) FILTER(WHERE k.kind=''received''),0)<>0
 OR coalesce(sum(delta) FILTER(WHERE k.kind=''system''),0)<>0 OR coalesce(sum(delta) FILTER(WHERE k.kind=''unknown''),0)<>0
 ORDER BY d.conversation_id,b.scope,b.period
 ON CONFLICT(conversation_id,scope,period) DO UPDATE SET
 sent=crm.message_stat_counts.sent+excluded.sent,received=crm.message_stat_counts.received+excluded.received,
 system=crm.message_stat_counts.system+excluded.system,unknown=crm.message_stat_counts.unknown+excluded.unknown';
 RETURN NULL;
END $$;
CREATE TRIGGER message_stat_i AFTER INSERT ON crm.message_search REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.project_message_stat_counts();
CREATE TRIGGER message_stat_u AFTER UPDATE ON crm.message_search REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.project_message_stat_counts();
CREATE TRIGGER message_stat_d AFTER DELETE ON crm.message_search REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.project_message_stat_counts();
CREATE TRIGGER cache_i AFTER INSERT ON crm.message_stat_counts REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''messages'' domain,coalesce(c.profile_id,c.id) subject FROM changed m JOIN crm.conversations c ON c.id=m.conversation_id');
CREATE TRIGGER cache_u AFTER UPDATE ON crm.message_stat_counts REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''messages'' domain,coalesce(c.profile_id,c.id) subject FROM changed m JOIN crm.conversations c ON c.id=m.conversation_id');
CREATE TRIGGER cache_d AFTER DELETE ON crm.message_stat_counts REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations('SELECT ''messages'' domain,coalesce(c.profile_id,c.id) subject FROM changed m JOIN crm.conversations c ON c.id=m.conversation_id');
CREATE OR REPLACE VIEW crm.page_contact_stats AS
 SELECT c.profile_id,min(c.first_message_at) FILTER(WHERE c.conversation_type='direct') first_contact_at,
 max(c.last_message_at) FILTER(WHERE c.conversation_type='direct') last_contact_at,
 max(c.last_message_at) FILTER(WHERE c.conversation_type='group') last_group_at,
 coalesce(sum(s.sent+s.received) FILTER(WHERE c.conversation_type='direct'),0)::bigint direct_message_count,
 coalesce(sum(s.sent+s.received) FILTER(WHERE c.conversation_type='group'),0)::bigint group_message_count
 FROM crm.conversations c LEFT JOIN crm.message_stat_counts s ON s.conversation_id=c.id AND s.scope='all'
 WHERE c.profile_id IS NOT NULL GROUP BY c.profile_id;
CREATE OR REPLACE VIEW crm.page_profile_months AS
 SELECT c.profile_id,s.period AS month,sum(s.sent)::bigint sent,sum(s.received)::bigint received
 FROM crm.message_stat_counts s JOIN crm.conversations c ON c.id=s.conversation_id
 WHERE s.scope='month:America/Chicago' AND c.profile_id IS NOT NULL AND c.conversation_type='direct'
 GROUP BY c.profile_id,s.period;
UPDATE crm.cache_versions SET version=gen_random_uuid() WHERE domain='messages';

CREATE VIEW crm.message_days_presented AS
 SELECT conversation_id,CASE WHEN scope='day:America/Chicago' THEN period::date END AS day,sent,received
 FROM crm.message_stat_counts WHERE scope='day:America/Chicago';
