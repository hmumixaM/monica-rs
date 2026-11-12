-- Page reads use current facts and bounded day/month counters, never revision history.
-- Transition tables aggregate each batch before touching shared counters.
CREATE TABLE crm.conversation_days (
 conversation_id uuid NOT NULL REFERENCES crm.conversations(id) ON DELETE CASCADE,
 day date NOT NULL, sent bigint NOT NULL DEFAULT 0, received bigint NOT NULL DEFAULT 0,
 PRIMARY KEY(conversation_id,day)
);
CREATE INDEX conversation_days_day ON crm.conversation_days(day,conversation_id);
CREATE FUNCTION crm.page_message_days() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;
BEGIN
 source_sql := CASE TG_OP
  WHEN 'INSERT' THEN 'SELECT conversation_id,occurred_at,sender,account,1::bigint AS delta FROM added WHERE NOT tombstone'
  WHEN 'DELETE' THEN 'SELECT conversation_id,occurred_at,sender,account,-1::bigint AS delta FROM removed WHERE NOT tombstone'
  ELSE 'SELECT conversation_id,occurred_at,sender,account,1::bigint AS delta FROM added WHERE NOT tombstone UNION ALL SELECT conversation_id,occurred_at,sender,account,-1::bigint AS delta FROM removed WHERE NOT tombstone' END;
 EXECUTE 'INSERT INTO crm.conversation_days(conversation_id,day,sent,received)
 SELECT conversation_id,(occurred_at AT TIME ZONE ''America/Chicago'')::date,
 coalesce(sum(delta) FILTER(WHERE sender=account),0),coalesce(sum(delta) FILTER(WHERE sender<>account),0)
 FROM ('||source_sql||') d GROUP BY 1,2 ORDER BY 1,2
 ON CONFLICT(conversation_id,day) DO UPDATE SET sent=crm.conversation_days.sent+excluded.sent,received=crm.conversation_days.received+excluded.received';
 RETURN NULL;
END $$;
CREATE TRIGGER page_message_days_insert AFTER INSERT ON crm.message_search REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_message_days();
CREATE TRIGGER page_message_days_update AFTER UPDATE ON crm.message_search REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_message_days();
CREATE TRIGGER page_message_days_delete AFTER DELETE ON crm.message_search REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.page_message_days();
INSERT INTO crm.conversation_days SELECT conversation_id,(occurred_at AT TIME ZONE 'America/Chicago')::date,count(*) FILTER(WHERE sender=account),count(*) FILTER(WHERE sender<>account) FROM crm.message_search WHERE NOT tombstone GROUP BY 1,2;

CREATE TABLE crm.contact_days (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),day date NOT NULL,kind text NOT NULL,
 count bigint NOT NULL,PRIMARY KEY(profile_id,day,kind)
);
CREATE INDEX contact_days_day ON crm.contact_days(day,profile_id);
CREATE FUNCTION crm.page_contact_days() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;
BEGIN
 source_sql := CASE TG_OP WHEN 'INSERT' THEN 'SELECT profile_id,occurred_at,kind,1::bigint AS delta FROM added WHERE content->>''is_deleted'' IS DISTINCT FROM ''true'''
 WHEN 'DELETE' THEN 'SELECT profile_id,occurred_at,kind,-1::bigint AS delta FROM removed WHERE content->>''is_deleted'' IS DISTINCT FROM ''true'''
 ELSE 'SELECT profile_id,occurred_at,kind,1::bigint AS delta FROM added WHERE content->>''is_deleted'' IS DISTINCT FROM ''true'' UNION ALL SELECT profile_id,occurred_at,kind,-1::bigint AS delta FROM removed WHERE content->>''is_deleted'' IS DISTINCT FROM ''true''' END;
 EXECUTE 'INSERT INTO crm.contact_days SELECT profile_id,(occurred_at AT TIME ZONE ''America/Chicago'')::date,kind,sum(delta)
 FROM ('||source_sql||') d WHERE profile_id IS NOT NULL AND occurred_at IS NOT NULL GROUP BY 1,2,3 ORDER BY 1,2,3
 ON CONFLICT(profile_id,day,kind) DO UPDATE SET count=crm.contact_days.count+excluded.count';
 RETURN NULL;
END $$;
CREATE TRIGGER page_contact_days_insert AFTER INSERT ON crm.contact_events REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_contact_days();
CREATE TRIGGER page_contact_days_update AFTER UPDATE ON crm.contact_events REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_contact_days();
CREATE TRIGGER page_contact_days_delete AFTER DELETE ON crm.contact_events REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.page_contact_days();
INSERT INTO crm.contact_days SELECT profile_id,(occurred_at AT TIME ZONE 'America/Chicago')::date,kind,count(*) FROM crm.contact_events WHERE profile_id IS NOT NULL AND occurred_at IS NOT NULL AND content->>'is_deleted' IS DISTINCT FROM 'true' GROUP BY 1,2,3;

CREATE TABLE crm.interaction_months (
 profile_id uuid NOT NULL REFERENCES crm.profiles(id),post_id uuid NOT NULL REFERENCES crm.timeline_events(id),
 month text NOT NULL,count bigint NOT NULL,PRIMARY KEY(profile_id,month,post_id)
);
CREATE FUNCTION crm.page_interaction_months() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;
BEGIN
 source_sql := CASE TG_OP WHEN 'INSERT' THEN 'SELECT *,1::bigint AS delta FROM added'
 WHEN 'DELETE' THEN 'SELECT *,-1::bigint AS delta FROM removed'
 ELSE 'SELECT *,1::bigint AS delta FROM added UNION ALL SELECT *,-1::bigint AS delta FROM removed' END;
 EXECUTE 'INSERT INTO crm.interaction_months SELECT author_profile_id,post_id,to_char(occurred_at AT TIME ZONE ''America/Chicago'',''YYYY-MM''),sum(delta)
 FROM ('||source_sql||') d WHERE author_profile_id IS NOT NULL AND occurred_at IS NOT NULL AND NOT is_deleted AND interaction_type IN (''like'',''comment'',''reply'') GROUP BY 1,2,3 ORDER BY 1,3,2
 ON CONFLICT(profile_id,month,post_id) DO UPDATE SET count=crm.interaction_months.count+excluded.count';
 RETURN NULL;
END $$;
CREATE TRIGGER page_interaction_insert AFTER INSERT ON crm.social_interactions REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_interaction_months();
CREATE TRIGGER page_interaction_update AFTER UPDATE ON crm.social_interactions REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_interaction_months();
CREATE TRIGGER page_interaction_delete AFTER DELETE ON crm.social_interactions REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.page_interaction_months();
INSERT INTO crm.interaction_months SELECT author_profile_id,post_id,to_char(occurred_at AT TIME ZONE 'America/Chicago','YYYY-MM'),count(*) FROM crm.social_interactions WHERE author_profile_id IS NOT NULL AND occurred_at IS NOT NULL AND NOT is_deleted AND interaction_type IN ('like','comment','reply') GROUP BY 1,2,3;

CREATE TABLE crm.page_posts (
 id uuid PRIMARY KEY REFERENCES crm.timeline_events(id) ON DELETE CASCADE,
 profile_id uuid REFERENCES crm.profiles(id),provider text NOT NULL,kind text NOT NULL,
 occurred_at timestamptz,text text NOT NULL,location jsonb,coordinate_format text,
 generation bigint NOT NULL,media_count integer NOT NULL DEFAULT 0
);
CREATE INDEX page_posts_recent ON crm.page_posts(occurred_at DESC,id);
CREATE INDEX page_posts_profile ON crm.page_posts(profile_id,occurred_at DESC,id);
CREATE FUNCTION crm.page_post_current() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.status='deleted' THEN DELETE FROM crm.page_posts WHERE id=NEW.id;
 ELSE INSERT INTO crm.page_posts(id,profile_id,provider,kind,occurred_at,text,location,coordinate_format,generation,media_count)
 VALUES(NEW.id,NEW.profile_id,NEW.provider,NEW.event_type,NEW.occurred_at,
 coalesce(NEW.content->>'text',NEW.content->>'caption',NEW.content->>'summary',NEW.content->>'body',''),
 NEW.content->'location',NEW.content->>'coordinate_format',NEW.version,
 CASE WHEN jsonb_typeof(NEW.content->'media')='array' THEN jsonb_array_length(NEW.content->'media') ELSE 0 END)
 ON CONFLICT(id) DO UPDATE SET profile_id=excluded.profile_id,provider=excluded.provider,kind=excluded.kind,occurred_at=excluded.occurred_at,text=excluded.text,location=excluded.location,coordinate_format=excluded.coordinate_format,generation=excluded.generation,media_count=excluded.media_count;
 END IF; RETURN NULL;
END $$;
CREATE TRIGGER page_post_current AFTER INSERT OR UPDATE ON crm.timeline_events FOR EACH ROW EXECUTE FUNCTION crm.page_post_current();
INSERT INTO crm.page_posts SELECT id,profile_id,provider,event_type,occurred_at,coalesce(content->>'text',content->>'caption',content->>'summary',content->>'body',''),content->'location',content->>'coordinate_format',version,CASE WHEN jsonb_typeof(content->'media')='array' THEN jsonb_array_length(content->'media') ELSE 0 END FROM crm.timeline_events WHERE status<>'deleted';

CREATE TABLE crm.community_names (community_id text PRIMARY KEY CHECK(length(community_id) BETWEEN 1 AND 160),label text NOT NULL CHECK(length(label) BETWEEN 1 AND 40),updated_at timestamptz NOT NULL DEFAULT now());

CREATE VIEW crm.page_contact_stats AS
 SELECT profile_id,min(first_message_at) FILTER(WHERE conversation_type='direct') first_contact_at,
 max(last_message_at) FILTER(WHERE conversation_type='direct') last_contact_at,
 max(last_message_at) FILTER(WHERE conversation_type='group') last_group_at,
 coalesce(sum(message_count) FILTER(WHERE conversation_type='direct'),0)::bigint direct_message_count,
 coalesce(sum(message_count) FILTER(WHERE conversation_type='group'),0)::bigint group_message_count
 FROM crm.conversations WHERE profile_id IS NOT NULL GROUP BY profile_id;
CREATE VIEW crm.page_profile_months AS
 SELECT c.profile_id,m.month,sum(m.sent)::bigint sent,sum(m.received)::bigint received
 FROM crm.message_months m JOIN crm.conversations c ON c.id=m.conversation_id
 WHERE c.profile_id IS NOT NULL AND c.conversation_type='direct' GROUP BY c.profile_id,m.month;

-- Historical audit totals are aggregated per object/profile to avoid a global
-- counter lock. Page reads sum compact counters rather than history tables.
CREATE TABLE crm.page_entity_counts(metric text NOT NULL,subject uuid NOT NULL,value bigint NOT NULL,PRIMARY KEY(metric,subject));
CREATE FUNCTION crm.page_entity_count() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;scope text=TG_ARGV[1];metric text=TG_ARGV[0];predicate text=coalesce(nullif(TG_ARGV[2],''),'true');
BEGIN
 source_sql := CASE TG_OP WHEN 'INSERT' THEN format('SELECT %I subject,1::bigint delta FROM added WHERE %s',scope,predicate)
 WHEN 'DELETE' THEN format('SELECT %I subject,-1::bigint delta FROM removed WHERE %s',scope,predicate)
 ELSE format('SELECT %I subject,1::bigint delta FROM added WHERE %s UNION ALL SELECT %I subject,-1::bigint delta FROM removed WHERE %s',scope,predicate,scope,predicate) END;
 EXECUTE 'INSERT INTO crm.page_entity_counts SELECT $1,subject,sum(delta) FROM ('||source_sql||') x GROUP BY subject ORDER BY subject ON CONFLICT(metric,subject) DO UPDATE SET value=crm.page_entity_counts.value+excluded.value' USING metric;
 RETURN NULL;
END $$;
DO $$DECLARE spec text[];provider text;BEGIN
 FOREACH spec SLICE 1 IN ARRAY ARRAY[
 ['crm','field_revisions','revisions','profile_id','true'],
 ['crm','field_revisions','address_revisions','profile_id','field_key ~ ''(address|addresses)$''']
 ] LOOP
 EXECUTE format('CREATE TRIGGER page_count_%s_i AFTER INSERT ON %I.%I REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_entity_count(%L,%L,%L)',spec[3],spec[1],spec[2],spec[3],spec[4],spec[5]);
 EXECUTE format('CREATE TRIGGER page_count_%s_d AFTER DELETE ON %I.%I REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.page_entity_count(%L,%L,%L)',spec[3],spec[1],spec[2],spec[3],spec[4],spec[5]);
 EXECUTE format('INSERT INTO crm.page_entity_counts SELECT %L,%I,count(*) FROM %I.%I WHERE %s GROUP BY %I',spec[3],spec[4],spec[1],spec[2],spec[5],spec[4]);
 END LOOP;
 FOREACH provider IN ARRAY ARRAY['wechat','instagram','linkedin'] LOOP
 FOREACH spec SLICE 1 IN ARRAY ARRAY[['observations','observations'],['revisions','source_revisions']] LOOP
 EXECUTE format('CREATE TRIGGER page_count_i AFTER INSERT ON %I.%I REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.page_entity_count(%L,''object_id'',''true'')',provider,spec[1],provider||'.'||spec[2]);
 EXECUTE format('CREATE TRIGGER page_count_d AFTER DELETE ON %I.%I REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.page_entity_count(%L,''object_id'',''true'')',provider,spec[1],provider||'.'||spec[2]);
 EXECUTE format('INSERT INTO crm.page_entity_counts SELECT %L,object_id,count(*) FROM %I.%I GROUP BY object_id',provider||'.'||spec[2],provider,spec[1]);
 END LOOP;END LOOP;
END $$;
