-- Freeze a whole requested window locally, then send bounded cached parts.
-- Immutable evidence and cached parts need no explicit source-row locks.
CREATE TABLE crm.ai_week_runs (
 job_id uuid PRIMARY KEY REFERENCES crm.ai_jobs(id) ON DELETE CASCADE,
 source_hash text NOT NULL,source_count bigint NOT NULL,
 captured_at timestamptz NOT NULL DEFAULT now(),ready_summary jsonb,
 pruned boolean NOT NULL DEFAULT false
);
CREATE TABLE crm.ai_week_evidence (
 job_id uuid NOT NULL REFERENCES crm.ai_week_runs(job_id) ON DELETE CASCADE,
 source_key text NOT NULL,source_id uuid NOT NULL,profile_id uuid NOT NULL,
 kind text NOT NULL,bucket text NOT NULL,payload_hash text NOT NULL,payload jsonb NOT NULL,
 PRIMARY KEY(job_id,source_key)
);
CREATE INDEX ON crm.ai_week_evidence(job_id,bucket,source_key);
CREATE INDEX ON crm.ai_week_evidence(job_id,source_id,profile_id);
CREATE TABLE crm.ai_week_buckets (
 job_id uuid NOT NULL REFERENCES crm.ai_week_runs(job_id) ON DELETE CASCADE,
 bucket text NOT NULL,output jsonb NOT NULL,source_count bigint NOT NULL,
 PRIMARY KEY(job_id,bucket)
);
CREATE TABLE crm.ai_week_parts (
 cache_key text PRIMARY KEY,output jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON crm.ai_week_runs(captured_at,job_id);
CREATE TABLE crm.ai_week_refresh (
 kind text NOT NULL CHECK(kind IN ('ai.person_week','ai.digest')),subject text NOT NULL,
 version uuid NOT NULL DEFAULT gen_random_uuid(),requested_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(kind,subject)
);
CREATE TABLE crm.ai_social_observed (
 event_id uuid PRIMARY KEY REFERENCES crm.timeline_events(id) ON DELETE CASCADE,
 first_observed_at timestamptz,checked_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON crm.ai_social_observed(first_observed_at,event_id) WHERE first_observed_at IS NOT NULL;
CREATE TABLE crm.ai_social_observed_pending (
 event_id uuid PRIMARY KEY REFERENCES crm.timeline_events(id) ON DELETE CASCADE,
 version uuid NOT NULL DEFAULT gen_random_uuid()
);
CREATE FUNCTION crm.queue_social_observed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO crm.ai_social_observed_pending(event_id) VALUES(NEW.id)
 ON CONFLICT(event_id) DO UPDATE SET version=gen_random_uuid();
 RETURN NULL;
END $$;
CREATE TRIGGER social_observed AFTER INSERT OR UPDATE OF object_id,source_account_id,connection_id,external_id,version
 ON crm.timeline_events FOR EACH ROW EXECUTE FUNCTION crm.queue_social_observed();
INSERT INTO crm.ai_social_observed_pending(event_id) SELECT id FROM crm.timeline_events;

-- Only retained source timestamps count as discovery. Never substitute now(),
-- last_seen_at, migration time, or the date an archived result was copied.
CREATE FUNCTION crm.ai_first_social_observation(event uuid) RETURNS timestamptz LANGUAGE plpgsql STABLE AS $$
DECLARE e crm.timeline_events;stamp timestamptz;account_external text;
BEGIN
 SELECT * INTO e FROM crm.timeline_events WHERE id=event;
 IF NOT FOUND OR e.provider NOT IN ('wechat','instagram','linkedin') THEN RETURN NULL;END IF;
 IF e.connection_id IS NOT NULL THEN
  EXECUTE format('SELECT min(observed_at) FROM %I.connection_revision_refs WHERE connection_id=$1 AND external_id=$2',e.provider)
   INTO stamp USING e.connection_id,e.external_id;
 ELSE
  EXECUTE format('SELECT min(observed_at) FROM %I.revision_metadata WHERE object_id IN ($1,$2)',e.provider)
   INTO stamp USING e.object_id,e.id;
  IF stamp IS NULL AND e.source_account_id IS NOT NULL THEN
   SELECT external_id INTO account_external FROM crm.source_accounts WHERE id=e.source_account_id;
   EXECUTE format('SELECT min(m.observed_at) FROM %I.objects o JOIN %I.accounts a ON a.id=o.account_id JOIN %I.revision_metadata m ON m.object_id=o.id WHERE a.external_id=$1 AND o.external_id=$2 AND o.object_kind=$3',e.provider,e.provider,e.provider)
    INTO stamp USING account_external,e.external_id,e.event_type;
  END IF;
 END IF;
 RETURN stamp;
END $$;
CREATE INDEX ON crm.message_search(occurred_at,message_id) WHERE NOT tombstone;
CREATE INDEX ON crm.field_revisions(observed_at,profile_id,id);
CREATE INDEX ON crm.source_structured_revisions(observed_at,id);

-- Compact source-policy fence; checking this between provider calls does not
-- repeatedly hash the whole captured window.
CREATE FUNCTION crm.ai_week_source_policy() RETURNS text LANGUAGE sql STABLE AS $$
 SELECT encode(sha256(convert_to(coalesce(jsonb_object_agg(id::text,
  jsonb_build_array(enabled,generation)),'{}'::jsonb)::text,'UTF8')),'hex')
 FROM crm.source_connections
$$;

CREATE FUNCTION crm.ai_week_sources(first_day date,wanted_person uuid,chats boolean,notes boolean)
RETURNS TABLE(source_key text,payload jsonb) LANGUAGE sql STABLE AS $$
 WITH bounds AS (
  SELECT first_day::timestamp AT TIME ZONE 'America/Chicago' start_at,
   (first_day+7)::timestamp AT TIME ZONE 'America/Chicago' end_at
 ), evidence AS (
 SELECT 'social:'||t.id::text identity,
  jsonb_build_object('id',t.id,'profile_id',p.id,'kind',t.event_type,'provider',t.provider,
   'text',coalesce(t.content->>'text',t.content->>'caption',''),'at',t.occurred_at,
   'version',t.version,'temporal_kind','occurred','discovered',false,
   'visual',crm.ai_social_visual(t.id),'citation','/timeline?event='||t.id::text) payload
 FROM crm.timeline_events t JOIN crm.memory_legacy_eligible_profiles p ON p.id=t.profile_id,bounds b
 WHERE t.status<>'deleted' AND (wanted_person IS NULL OR p.id=wanted_person)
  AND t.occurred_at>=b.start_at AND t.occurred_at<b.end_at
  AND (t.connection_id IS NULL OR EXISTS(SELECT 1 FROM crm.connection_evidence c
   JOIN crm.source_connections s ON s.id=c.connection_id
   WHERE c.id=t.id AND c.included AND NOT c.deleted AND s.enabled))
 UNION ALL
 SELECT 'discovered:'||t.id::text,
  jsonb_build_object('id',t.id,'profile_id',p.id,'kind',t.event_type,'provider',t.provider,
   'text',coalesce(t.content->>'text',t.content->>'caption',''),'at',t.occurred_at,
   'discovered_at',o.first_observed_at,'version',t.version,'temporal_kind','discovered',
   'discovered',true,'visual',crm.ai_social_visual(t.id),'citation','/timeline?event='||t.id::text)
 FROM crm.ai_social_observed o JOIN crm.timeline_events t ON t.id=o.event_id
 JOIN crm.memory_legacy_eligible_profiles p ON p.id=t.profile_id,bounds b
 WHERE o.first_observed_at>=b.start_at AND o.first_observed_at<b.end_at AND t.status<>'deleted'
  AND (t.occurred_at IS NULL OR t.occurred_at<b.start_at OR t.occurred_at>=b.end_at)
  AND (wanted_person IS NULL OR p.id=wanted_person)
  AND (t.connection_id IS NULL OR EXISTS(SELECT 1 FROM crm.connection_evidence c
   JOIN crm.source_connections s ON s.id=c.connection_id
   WHERE c.id=t.id AND c.included AND NOT c.deleted AND s.enabled))
 UNION ALL
 SELECT 'message:'||m.message_id::text,
  jsonb_build_object('id',m.message_id,'profile_id',p.id,'kind',c.conversation_type,
   'provider',m.provider,'text',m.body,'at',m.occurred_at,'conversation_id',m.conversation_id,
   'sender',m.sender,'sender_profile_id',coalesce(m.sender_profile_id,l.profile_id),
   'direction',coalesce(m.direction,CASE WHEN m.sender=m.account THEN 'sent' ELSE 'received' END),
   'speaker',CASE WHEN c.conversation_type='group' THEN 'TA' WHEN m.direction='unknown' THEN '未知'
    WHEN m.direction='sent' OR (m.direction IS NULL AND m.sender=m.account) THEN '我' ELSE 'TA' END,
   'group',CASE WHEN c.conversation_type='group' THEN coalesce(c.source_display_name,c.external_id) END,
   'temporal_kind','occurred','discovered',false)
 FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id
 LEFT JOIN crm.source_account_links l ON l.account_id=m.sender_account_id
 JOIN crm.memory_legacy_eligible_profiles p ON p.id=CASE WHEN c.conversation_type='group'
  THEN coalesce(m.sender_profile_id,l.profile_id) ELSE c.profile_id END,bounds b
 WHERE chats AND NOT m.tombstone AND (wanted_person IS NULL OR p.id=wanted_person)
  AND m.occurred_at>=b.start_at AND m.occurred_at<b.end_at
  AND (c.connection_id IS NULL OR EXISTS(SELECT 1 FROM crm.connection_evidence e
   JOIN crm.source_connections sc ON sc.id=e.connection_id AND sc.enabled
   WHERE e.connection_id=c.connection_id AND e.message_id=m.message_id
    AND e.included AND NOT e.deleted AND (sc.provider<>'mail' OR e.category='personal')))
 UNION ALL
 SELECT 'activity:'||a.id::text||':'||p.id::text,
  jsonb_build_object('id',a.id,'profile_id',p.id,'kind','note','activity_type',a.activity_type,
   'text',coalesce(a.body,a.title),'title',a.title,'at',a.occurred_at,'version',a.content_hash,
   'temporal_kind','occurred','discovered',false)
 FROM crm.activities a JOIN crm.activity_participants ap ON ap.activity_id=a.id
 JOIN crm.memory_legacy_eligible_profiles p ON p.id=ap.profile_id,bounds b
 WHERE notes AND a.activity_type NOT IN ('wechat_direct_summary','wechat_group_summary')
  AND (wanted_person IS NULL OR p.id=wanted_person)
  AND a.occurred_at>=b.start_at AND a.occurred_at<b.end_at
 UNION ALL
 SELECT 'profile-update:'||r.id::text,
  jsonb_build_object('id',r.id,'profile_id',p.id,'kind','profile','field_key',r.field_key,
   'text',r.value,'at',r.observed_at,'occurred_at',NULL,'temporal_kind','profile_update',
   'discovered',true,'citation','/profiles/'||p.id::text)
 FROM crm.field_revisions r JOIN crm.memory_legacy_eligible_profiles p ON p.id=r.profile_id,bounds b
 WHERE ((r.source_type='manual' AND r.operation<>'import') OR EXISTS(
   SELECT 1 FROM crm.source_connections c WHERE c.id::text=split_part(r.source_record_id,':',1)
    AND c.provider=r.source_type AND c.enabled))
  AND (wanted_person IS NULL OR p.id=wanted_person)
  AND r.observed_at>=b.start_at AND r.observed_at<b.end_at
  AND (r.field_key LIKE 'monica.%' OR r.field_key=ANY(ARRAY[
   'residence','hometown','company','job','school','email','phone','food','interests','family',
   'personal.residence','personal.hometown','employment.company','employment.title','education.school',
   'contact.email','contact.phone','personal.food','personal.interests','personal.family',
   'apple_contacts.emails','apple_contacts.phones','apple_contacts.organization','apple_contacts.job_title','apple_contacts.birthday'])
   OR r.field_key LIKE 'linkedin.%' OR r.field_key LIKE 'wechat.%')
 UNION ALL
 SELECT 'structured-update:'||r.id::text||':'||p.id::text,
  jsonb_build_object('id',r.id,'profile_id',p.id,'kind','profile','structured_kind',r.fact_type,
   'text',r.value,'at',r.observed_at,'occurred_at',NULL,'temporal_kind','profile_update',
   'discovered',true,'citation','/profiles/'||p.id::text)
 FROM crm.source_structured_revisions r JOIN crm.profile_structured_current c ON c.revision_id=r.id
 JOIN crm.memory_legacy_eligible_profiles p ON p.id=c.profile_id,bounds b
 WHERE (wanted_person IS NULL OR p.id=wanted_person) AND r.observed_at>=b.start_at AND r.observed_at<b.end_at
  AND EXISTS(SELECT 1 FROM crm.source_connections s
   WHERE s.id::text=split_part(r.source_record_id,':',1) AND s.provider=r.source_type AND s.enabled)
 ) SELECT encode(sha256(convert_to(identity,'UTF8')),'hex'),payload FROM evidence
$$;
