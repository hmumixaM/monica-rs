-- Legacy input addresses are already the primary key. Use that complete
-- address for removal instead of rescanning all of a person's source history.
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
 DELETE FROM crm.memory_inputs m USING crm.memory_legacy_owners o WHERE o.kind=record_kind AND o.entity_id=ANY(record_ids) AND m.profile_id=o.profile_id
 AND m.source_key=encode(sha256(convert_to('legacy:'||o.kind||':'||o.entity_id::text,'UTF8')),'hex')
 AND m.source_id='legacy:'||o.kind||':'||o.entity_id::text
 AND NOT EXISTS(SELECT 1 FROM wanted w WHERE w.profile_id=o.profile_id AND w.entity_id=o.entity_id) RETURNING m.profile_id
 ), saved AS (
 INSERT INTO crm.memory_legacy_owners SELECT kind,entity_id,profile_id FROM wanted ON CONFLICT DO NOTHING RETURNING profile_id
 ), forgotten AS (
 DELETE FROM crm.memory_legacy_owners o WHERE o.kind=record_kind AND o.entity_id=ANY(record_ids) AND NOT EXISTS(SELECT 1 FROM wanted w WHERE w.profile_id=o.profile_id AND w.entity_id=o.entity_id) RETURNING profile_id
 ) UPDATE crm.memory_heads SET generation=generation+1,updated_at=now() WHERE profile_id IN(SELECT profile_id FROM inserted UNION SELECT profile_id FROM removed);
END $$;
