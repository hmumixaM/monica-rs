-- References only: source text stays in message_search/timeline_events/activities.
CREATE TABLE crm.memory_legacy_owners (
 kind text NOT NULL,entity_id uuid NOT NULL,profile_id uuid NOT NULL REFERENCES crm.profiles(id),
 PRIMARY KEY(kind,entity_id,profile_id)
);
CREATE INDEX ON crm.memory_legacy_owners(profile_id,kind,entity_id);
CREATE INDEX ON crm.message_search(sender_account_id,message_id);
CREATE INDEX ON crm.message_search(sender_profile_id,message_id) WHERE sender_profile_id IS NOT NULL;
CREATE VIEW crm.memory_legacy_current AS
 SELECT e.* FROM (
 SELECT 'message'::text kind,m.message_id entity_id,
 CASE WHEN c.conversation_type='group' THEN coalesce(m.sender_profile_id,l.profile_id) ELSE c.profile_id END profile_id,
 'chat'::text source_kind,true private,
 jsonb_build_object('id',m.message_id,'kind','message','provider',m.provider,'at',m.occurred_at,'conversation_id',m.conversation_id,'text',m.body) payload
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
CREATE FUNCTION crm.memory_sync_legacy(record_kind text,record_ids uuid[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE owners uuid[];
BEGIN
 IF cardinality(record_ids)>1000 THEN RAISE EXCEPTION 'legacy Memory batch exceeds 1000'; END IF;
 SELECT array_agg(profile_id ORDER BY profile_id) INTO owners FROM (
 SELECT profile_id FROM crm.memory_legacy_owners WHERE kind=record_kind AND entity_id=ANY(record_ids)
 UNION SELECT profile_id FROM crm.memory_legacy_current WHERE kind=record_kind AND entity_id=ANY(record_ids)) p;
 IF owners IS NULL THEN RETURN; END IF;
 INSERT INTO crm.memory_heads(profile_id) SELECT unnest(owners) ON CONFLICT DO NOTHING;
 PERFORM profile_id FROM crm.memory_heads WHERE profile_id=ANY(owners) ORDER BY profile_id FOR UPDATE;
 WITH wanted AS MATERIALIZED (
 SELECT c.*,encode(sha256(convert_to('legacy:'||kind||':'||entity_id::text,'UTF8')),'hex') source_key,
 'legacy:'||kind||':'||entity_id::text source_id FROM crm.memory_legacy_current c WHERE kind=record_kind AND entity_id=ANY(record_ids)
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
-- Linking can affect millions of messages. Persist a keyset sweep and fence
-- those people's Memory publication until every bounded page has caught up.
CREATE TABLE crm.memory_legacy_scopes (
 scope text NOT NULL CHECK(scope IN ('account','profile')),id uuid NOT NULL,
 after_kind text NOT NULL DEFAULT '',after_id uuid NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
 profiles uuid[] NOT NULL,updated_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(scope,id)
);
CREATE INDEX ON crm.memory_legacy_scopes USING gin(profiles);
CREATE FUNCTION crm.memory_legacy_profile_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO crm.memory_legacy_scopes(scope,id,profiles)
 SELECT 'profile',n.id,ARRAY[n.id] FROM added n JOIN removed o ON o.id=n.id
 WHERE (n.archived_at,n.profile_type) IS DISTINCT FROM (o.archived_at,o.profile_type)
 ORDER BY n.id ON CONFLICT(scope,id) DO UPDATE SET after_kind='',after_id='00000000-0000-0000-0000-000000000000',updated_at=now();
 RETURN NULL;
END $$;
CREATE TRIGGER memory_legacy_profile_state AFTER UPDATE ON crm.profiles REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.memory_legacy_profile_changed();
CREATE FUNCTION crm.memory_legacy_self_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;
BEGIN
 source_sql:=CASE TG_OP WHEN 'INSERT' THEN 'SELECT profile_id FROM added WHERE is_self' WHEN 'DELETE' THEN 'SELECT profile_id FROM removed WHERE is_self' ELSE 'SELECT profile_id FROM added WHERE is_self UNION SELECT profile_id FROM removed WHERE is_self' END;
 EXECUTE 'INSERT INTO crm.memory_legacy_scopes(scope,id,profiles) SELECT ''profile'',profile_id,ARRAY[profile_id] FROM ('||source_sql||') p GROUP BY profile_id ORDER BY profile_id ON CONFLICT(scope,id) DO UPDATE SET after_kind='''',after_id=''00000000-0000-0000-0000-000000000000'',updated_at=now()';
 RETURN NULL;
END $$;
CREATE TRIGGER memory_legacy_self_i AFTER INSERT ON crm.person_identifiers REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.memory_legacy_self_changed();
CREATE TRIGGER memory_legacy_self_u AFTER UPDATE ON crm.person_identifiers REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.memory_legacy_self_changed();
CREATE TRIGGER memory_legacy_self_d AFTER DELETE ON crm.person_identifiers REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.memory_legacy_self_changed();
CREATE FUNCTION crm.memory_legacy_scope_invalidate() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE people uuid[];
BEGIN
 SELECT array_agg(id ORDER BY id) INTO people FROM (SELECT DISTINCT unnest(profiles) id FROM added WHERE after_kind='') p;
 IF people IS NULL THEN RETURN NULL; END IF;
 INSERT INTO crm.memory_heads(profile_id) SELECT unnest(people) ON CONFLICT DO NOTHING;
 PERFORM profile_id FROM crm.memory_heads WHERE profile_id=ANY(people) ORDER BY profile_id FOR UPDATE;
 UPDATE crm.memory_heads SET generation=generation+1,updated_at=now() WHERE profile_id=ANY(people);
 RETURN NULL;
END $$;
CREATE TRIGGER memory_scope_i AFTER INSERT ON crm.memory_legacy_scopes REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.memory_legacy_scope_invalidate();
CREATE TRIGGER memory_scope_u AFTER UPDATE ON crm.memory_legacy_scopes REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.memory_legacy_scope_invalidate();
