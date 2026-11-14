-- Preserve existing configured defaults and credentials; never enable execution.
ALTER TABLE crm.ai_settings ADD COLUMN model_routes jsonb NOT NULL DEFAULT '{}'
 CHECK(jsonb_typeof(model_routes)='object');
-- Older results used the same model at every stage. NULL retains that identity
-- without rewriting the imported historical result or its generation time.
ALTER TABLE crm.memory_current ADD COLUMN chunk_model text;

-- Refresh only compact profile facts in bounded maintenance turns. Do not
-- rescan imported message histories merely to extend the field allow-list.
CREATE TABLE crm.memory_profile_refresh (
 profile_id uuid PRIMARY KEY REFERENCES crm.profiles(id) ON DELETE CASCADE
);
INSERT INTO crm.memory_profile_refresh(profile_id)
 SELECT DISTINCT c.profile_id FROM crm.field_current c JOIN crm.profiles p ON p.id=c.profile_id
 WHERE p.profile_type='person' AND p.archived_at IS NULL AND c.field_key IN (
  'linkedin.headline','linkedin.about','linkedin.location','wechat.signature','wechat.labels',
  'wechat.city','wechat.province','wechat.country','籍贯');

CREATE FUNCTION crm.ai_ocr_media_allowed(asset uuid,include_chat boolean) RETURNS boolean
LANGUAGE sql STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM crm.media_links l WHERE l.media_id=asset AND (
  (l.entity_type IN ('social_post','social_story','post','story')
   AND l.role ~ '^(media-[0-9]+(-poster)?|thumbnail-(media-|video-)?[0-9]+)$'
   AND EXISTS(SELECT 1 FROM crm.timeline_events e WHERE e.id=l.entity_id AND e.status<>'deleted'))
  OR (include_chat AND l.entity_type='message_event'
   AND EXISTS(SELECT 1 FROM crm.message_search m WHERE m.message_id=l.entity_id AND NOT m.tombstone))
 ))
$$;
