-- Keep the complete-window contract from 0073; fold only duplicate direct mail.
-- All account evidence, revisions, and attachments remain unchanged.
-- Fence captures made before logical mail copy folding. Existing saved results
-- remain readable; an in-flight old capture is refreshed before inference.
CREATE OR REPLACE FUNCTION crm.ai_week_source_policy() RETURNS text LANGUAGE sql STABLE AS $$
 SELECT encode(sha256(convert_to('mail-copy-v1:'||coalesce(jsonb_object_agg(id::text,
  jsonb_build_array(enabled,generation)),'{}'::jsonb)::text,'UTF8')),'hex')
 FROM crm.source_connections
$$;

CREATE OR REPLACE FUNCTION crm.ai_week_sources(first_day date,wanted_person uuid,chats boolean,notes boolean)
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
  -- Choose among this person's eligible copies within the requested week.
  -- Disabled or non-personal copies cannot suppress an eligible source.
  AND (m.provider<>'mail' OR c.conversation_type<>'direct' OR NOT EXISTS(
   SELECT 1 FROM crm.connection_evidence original
   JOIN crm.connection_evidence duplicate ON duplicate.dedup_key=original.dedup_key
   JOIN crm.message_search other ON other.message_id=duplicate.message_id
   JOIN crm.conversations other_room ON other_room.id=other.conversation_id
   JOIN crm.source_connections other_connection ON other_connection.id=duplicate.connection_id AND other_connection.enabled
   WHERE original.message_id=m.message_id AND original.connection_id=c.connection_id
    AND original.dedup_key IS NOT NULL AND duplicate.included AND NOT duplicate.deleted
    AND duplicate.kind='message' AND duplicate.category='personal'
    AND duplicate.connection_id=other_room.connection_id AND duplicate.conversation_id=other_room.id
    AND other_room.profile_id=p.id AND other_room.conversation_type='direct' AND other.provider='mail'
    AND NOT other.tombstone AND other.occurred_at>=b.start_at AND other.occurred_at<b.end_at
    AND (other.occurred_at,other.message_id)>(m.occurred_at,m.message_id)
  ))
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
