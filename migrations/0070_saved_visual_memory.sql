-- Saved results remain usable while execution is paused. Keep the visual
-- projection separate from private chat inputs and from face suggestions.
CREATE FUNCTION crm.ai_social_visual(event uuid) RETURNS jsonb LANGUAGE sql STABLE AS $$
 WITH selected AS (
  SELECT e.* FROM crm.timeline_events e
  WHERE e.id=event AND e.status='active'
   AND (e.connection_id IS NULL OR EXISTS(
    SELECT 1 FROM crm.connection_evidence c JOIN crm.source_connections s ON s.id=c.connection_id
    WHERE c.id=e.id AND c.included AND NOT c.deleted AND s.enabled))
 ), media AS (
  SELECT l.media_id,array_agg(DISTINCT l.role ORDER BY l.role) roles FROM selected e
  JOIN crm.media_links l ON l.entity_id=e.id
   AND l.entity_type IN ('social_post','social_story','post','story')
   AND l.role ~ '^(media-[0-9]+(-poster)?|thumbnail-(media-|video-)?[0-9]+)$'
  GROUP BY l.media_id
 ), saved AS (
  SELECT m.media_id,a.sha256,m.roles,
   CASE WHEN r.status='complete' OR r.generated_at IS NOT NULL THEN
    jsonb_build_object('summary',r.summary,'description',r.description,'text',r.text,
     'category',r.category,'model',r.model,'generated_at',r.generated_at,'input_hash',r.input_hash)
   END reading,
   (SELECT jsonb_agg(jsonb_build_object('face_id',f.id,'profile_id',p.id,'name',p.display_name,
      'confirmed_at',f.decided_at) ORDER BY f.id)
    FROM crm.faces f JOIN crm.profiles p ON p.id=f.profile_id
    WHERE f.media_id=m.media_id AND f.status='confirmed' AND p.archived_at IS NULL
     AND p.profile_type='person') confirmed_people
  FROM media m JOIN crm.media_assets a ON a.id=m.media_id
  LEFT JOIN crm.image_ocr_readings r ON r.media_id=m.media_id
 ), caption AS (
  SELECT jsonb_build_object('summary',r.summary,'items',r.items,'model',r.model,
   'generated_at',r.generated_at,'input_hash',r.input_hash) value
  FROM selected e JOIN crm.media_readings r ON r.entity_id=e.id
   AND r.entity_type=CASE WHEN e.event_type='story' THEN 'social_story' ELSE 'social_post' END
  WHERE (r.status='complete' OR r.generated_at IS NOT NULL)
   AND (coalesce(btrim(r.summary),'')<>'' OR r.items<>'[]'::jsonb)
 )
 SELECT jsonb_build_object('id',e.id,'kind',e.event_type,'provider',e.provider,
  'at',e.occurred_at,'text',coalesce(e.content->>'text',e.content->>'caption',''),
  'citation','/timeline?event='||e.id::text,'post_version',e.version,
  'saved_caption',(SELECT value FROM caption),
  'media',coalesce((SELECT jsonb_agg(to_jsonb(s) ORDER BY s.media_id) FROM saved s
   WHERE s.reading IS NOT NULL OR s.confirmed_people IS NOT NULL),'[]'::jsonb))
 FROM selected e WHERE EXISTS(SELECT 1 FROM caption)
  OR EXISTS(SELECT 1 FROM saved WHERE reading IS NOT NULL OR confirmed_people IS NOT NULL)
$$;

CREATE TABLE crm.memory_visual_owners (
 event_id uuid PRIMARY KEY,profile_id uuid NOT NULL REFERENCES crm.profiles(id)
);
CREATE INDEX ON crm.memory_visual_owners(profile_id,event_id);
CREATE TABLE crm.memory_visual_pending (
 event_id uuid PRIMARY KEY,profiles uuid[] NOT NULL,
 version uuid NOT NULL DEFAULT gen_random_uuid(),
 requested_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX ON crm.memory_visual_pending USING gin(profiles);
CREATE INDEX ON crm.memory_visual_pending(requested_at,event_id);
CREATE OR REPLACE VIEW crm.memory_legacy_pending AS
 SELECT unnest(profiles) profile_id FROM crm.memory_legacy_scopes
 UNION ALL SELECT unnest(profiles) FROM crm.memory_legacy_pending_records
 UNION ALL SELECT unnest(profiles) FROM crm.memory_visual_pending;

CREATE FUNCTION crm.queue_visual_memory(events uuid[],previous_profiles uuid[] DEFAULT '{}') RETURNS void
LANGUAGE sql AS $$
 INSERT INTO crm.memory_visual_pending(event_id,profiles)
 SELECT ids.id,ARRAY(SELECT DISTINCT p FROM (
  SELECT unnest(previous_profiles) p
  UNION ALL SELECT o.profile_id FROM crm.memory_visual_owners o WHERE o.event_id=ids.id
  UNION ALL SELECT t.profile_id FROM crm.timeline_events t WHERE t.id=ids.id
 ) people WHERE p IS NOT NULL ORDER BY p)
 FROM (SELECT DISTINCT unnest(events) id) ids ORDER BY ids.id
 ON CONFLICT(event_id) DO UPDATE SET
  profiles=ARRAY(SELECT DISTINCT unnest(crm.memory_visual_pending.profiles||excluded.profiles)),
  version=gen_random_uuid(),
  requested_at=clock_timestamp()
$$;

CREATE FUNCTION crm.visual_memory_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_row jsonb;new_row jsonb;ids uuid[];owners uuid[]='{}';media uuid[];
BEGIN
 IF TG_OP<>'INSERT' THEN old_row=to_jsonb(OLD); END IF;
 IF TG_OP<>'DELETE' THEN new_row=to_jsonb(NEW); END IF;
 IF TG_TABLE_NAME='timeline_events' THEN
  ids=ARRAY[coalesce(new_row->>'id',old_row->>'id')::uuid];
  owners=ARRAY[(old_row->>'profile_id')::uuid];
 ELSIF TG_TABLE_NAME='media_readings' THEN
  ids=ARRAY[(old_row->>'entity_id')::uuid,(new_row->>'entity_id')::uuid];
 ELSIF TG_TABLE_NAME='media_links' THEN
  SELECT array_agg(DISTINCT (r->>'entity_id')::uuid) INTO ids
  FROM unnest(ARRAY[old_row,new_row]) r
  WHERE r->>'entity_type' IN ('social_post','social_story','post','story')
   AND r->>'role' ~ '^(media-[0-9]+(-poster)?|thumbnail-(media-|video-)?[0-9]+)$';
 ELSIF TG_TABLE_NAME IN ('faces','image_ocr_readings') THEN
  media=ARRAY[(old_row->>'media_id')::uuid,(new_row->>'media_id')::uuid];
  SELECT array_agg(DISTINCT entity_id) INTO ids FROM crm.media_links
  WHERE media_id=ANY(media) AND entity_type IN ('social_post','social_story','post','story');
 ELSIF TG_TABLE_NAME IN ('profiles','person_identifiers') THEN
  owners=ARRAY[coalesce(new_row->>CASE WHEN TG_TABLE_NAME='profiles' THEN 'id' ELSE 'profile_id' END,
                      old_row->>CASE WHEN TG_TABLE_NAME='profiles' THEN 'id' ELSE 'profile_id' END)::uuid];
  SELECT array_agg(DISTINCT id) INTO ids FROM (
   SELECT t.id FROM crm.timeline_events t WHERE t.profile_id=ANY(owners)
   UNION SELECT l.entity_id FROM crm.faces f JOIN crm.media_links l ON l.media_id=f.media_id
    WHERE f.profile_id=ANY(owners) AND f.status='confirmed'
     AND l.entity_type IN ('social_post','social_story','post','story')
  ) related;
 ELSIF TG_TABLE_NAME='connection_evidence' THEN
  ids=ARRAY[coalesce(new_row->>'id',old_row->>'id')::uuid];
 ELSE
  SELECT array_agg(id ORDER BY id) INTO ids FROM crm.timeline_events
   WHERE connection_id=coalesce(new_row->>'id',old_row->>'id')::uuid;
 END IF;
 PERFORM crm.queue_visual_memory(array_remove(ids,NULL),owners);
 RETURN NULL;
END $$;
CREATE TRIGGER visual_timeline AFTER INSERT OR DELETE OR UPDATE OF profile_id,status,content,version
 ON crm.timeline_events FOR EACH ROW EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_media AFTER INSERT OR UPDATE OR DELETE ON crm.media_links
 FOR EACH ROW EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_ocr AFTER INSERT OR UPDATE OR DELETE ON crm.image_ocr_readings
 FOR EACH ROW EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_captions AFTER INSERT OR UPDATE OR DELETE ON crm.media_readings
 FOR EACH ROW EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_faces AFTER INSERT OR DELETE OR UPDATE OF status,profile_id,media_id,decided_at
 ON crm.faces FOR EACH ROW EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_people AFTER UPDATE OF display_name,archived_at,profile_type ON crm.profiles
 FOR EACH ROW WHEN (OLD.display_name IS DISTINCT FROM NEW.display_name
 OR OLD.archived_at IS DISTINCT FROM NEW.archived_at OR OLD.profile_type IS DISTINCT FROM NEW.profile_type)
 EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_self AFTER INSERT OR DELETE OR UPDATE OF is_self,profile_id ON crm.person_identifiers
 FOR EACH ROW EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_source AFTER UPDATE OF enabled ON crm.source_connections
 FOR EACH ROW WHEN (OLD.enabled IS DISTINCT FROM NEW.enabled) EXECUTE FUNCTION crm.visual_memory_changed();
CREATE TRIGGER visual_evidence AFTER UPDATE OF included,deleted ON crm.connection_evidence
 FOR EACH ROW WHEN (OLD.included IS DISTINCT FROM NEW.included OR OLD.deleted IS DISTINCT FROM NEW.deleted)
 EXECUTE FUNCTION crm.visual_memory_changed();

-- Seed references, not payloads: maintenance processes these in bounded turns.
INSERT INTO crm.memory_visual_pending(event_id,profiles)
 SELECT t.id,ARRAY[t.profile_id] FROM crm.timeline_events t WHERE t.profile_id IS NOT NULL
 AND (EXISTS(SELECT 1 FROM crm.media_readings r WHERE r.entity_id=t.id)
 OR EXISTS(SELECT 1 FROM crm.media_links l WHERE l.entity_id=t.id
  AND (EXISTS(SELECT 1 FROM crm.image_ocr_readings r WHERE r.media_id=l.media_id)
   OR EXISTS(SELECT 1 FROM crm.faces f WHERE f.media_id=l.media_id AND f.status='confirmed'))));
