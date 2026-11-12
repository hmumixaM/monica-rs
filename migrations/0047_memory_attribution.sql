-- Retain authoritative message author metadata in model-visible references.
CREATE OR REPLACE VIEW crm.memory_legacy_current AS
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
 ) e JOIN crm.profiles p ON p.id=e.profile_id AND p.profile_type='person' AND p.archived_at IS NULL
 WHERE NOT EXISTS(SELECT 1 FROM crm.person_identifiers i WHERE i.profile_id=p.id AND i.is_self);
