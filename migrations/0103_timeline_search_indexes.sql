-- Account lookups use exact, case-insensitive handles before expanding posts.
CREATE INDEX search_identity_username ON crm.identities(lower(username))
 WHERE status NOT IN ('invalid','merged');
CREATE INDEX search_source_username ON crm.source_accounts(lower(username));
CREATE INDEX search_wechat_profile_alias ON crm.field_revisions(lower(value #>> '{}'))
 WHERE field_key IN ('wechat.alias','wechat.nickname','wechat.remark') AND jsonb_typeof(value)='string';
CREATE INDEX search_instagram_post_username ON crm.timeline_events(lower(content->>'username'))
 WHERE provider='instagram' AND status NOT IN ('deleted','merged');

CREATE FUNCTION crm.instagram_search_mentions(content jsonb) RETURNS text[]
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
 SELECT coalesce(array_agg(DISTINCT lower(value #>> '{}')), '{}') FROM (
  SELECT jsonb_path_query(content,'$.caption_mentions[*].username') value
  UNION ALL SELECT jsonb_path_query(content,'$.caption_mentions[*]')
  UNION ALL SELECT jsonb_path_query(content,'$.tagged_users[*].username')
  UNION ALL SELECT jsonb_path_query(content,'$.tagged_users[*].user.username')
  UNION ALL SELECT jsonb_path_query(content,'$.tagged_users[*]')
  UNION ALL SELECT jsonb_path_query(content,'$.reel_mentions[*].user.username')
  UNION ALL SELECT jsonb_path_query(content,'$.archive_manifest.raw_metadata.reel_mentions[*].user.username')
  UNION ALL SELECT jsonb_path_query(content,'$.archive_manifest.raw_metadata.iphone_struct.reel_mentions[*].user.username')
 ) mentions WHERE jsonb_typeof(value)='string'
$$;
CREATE INDEX search_instagram_post_mentions ON crm.timeline_events USING gin(crm.instagram_search_mentions(content))
 WHERE provider='instagram' AND status NOT IN ('deleted','merged');
CREATE INDEX search_wechat_post_mentions
 ON crm.social_interactions((coalesce(content->>'target_external_id',metadata->>'target_external_id')))
 WHERE provider='wechat' AND interaction_type='mention' AND NOT is_deleted;

-- Reuse the message search grams: two CJK characters or three Latin letters/digits.
-- Historical per-post readings store OCR by image in items rather than a row per asset.
CREATE FUNCTION crm.post_reading_search_text(items jsonb) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
 SELECT string_agg(item->>'text', E'\n' ORDER BY position)
 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(items)='array' THEN items ELSE '[]'::jsonb END)
 WITH ORDINALITY AS reading(item,position) WHERE jsonb_typeof(item->'text')='string'
$$;
CREATE INDEX search_image_ocr_text ON crm.image_ocr_readings USING gin(crm.search_grams(text))
 WHERE status='complete' OR generated_at IS NOT NULL;
CREATE INDEX search_post_reading_text ON crm.media_readings USING gin(crm.search_grams(crm.post_reading_search_text(items)))
 WHERE status='complete' OR generated_at IS NOT NULL;

-- These sources can change a hit without changing the indexed post itself.
DO $$
DECLARE source record; operation text; transition text;
BEGIN
 FOR source IN SELECT * FROM (VALUES
  ('source_accounts', 'SELECT ''post-search'' domain,id subject FROM changed WHERE username IS NOT NULL OR provider=''wechat'''),
  ('social_interactions', 'SELECT ''post-search'' domain,post_id subject FROM changed WHERE provider=''wechat'' AND interaction_type=''mention'''),
  ('image_ocr_readings', 'SELECT ''post-search'' domain,media_id subject FROM changed WHERE text IS NOT NULL AND (status=''complete'' OR generated_at IS NOT NULL)'),
  ('media_readings', 'SELECT ''post-search'' domain,entity_id subject FROM changed WHERE items<>''[]''::jsonb AND (status=''complete'' OR generated_at IS NOT NULL)'),
  ('media_links', 'SELECT ''post-search'' domain,entity_id subject FROM changed WHERE entity_type IN (''social_post'',''social_story'',''post'',''story'')')
 ) tables(name, keys) LOOP
  FOREACH operation IN ARRAY ARRAY['INSERT','UPDATE','DELETE'] LOOP
   IF source.name='source_accounts' AND operation='UPDATE' THEN CONTINUE; END IF;
   transition := CASE operation WHEN 'INSERT' THEN 'NEW TABLE AS added'
     WHEN 'DELETE' THEN 'OLD TABLE AS removed' ELSE 'OLD TABLE AS removed NEW TABLE AS added' END;
   EXECUTE format('CREATE TRIGGER post_search_%s AFTER %s ON crm.%I REFERENCING %s FOR EACH STATEMENT EXECUTE FUNCTION crm.queue_cache_invalidations(%L)',
     lower(operation), operation, source.name, transition, source.keys);
  END LOOP;
 END LOOP;
END $$;

CREATE TRIGGER post_search_update AFTER UPDATE ON crm.source_accounts
 REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT
 EXECUTE FUNCTION crm.queue_cache_invalidations($keys$
   SELECT 'post-search' domain,id subject FROM (
    (SELECT id,provider,username,external_id FROM added EXCEPT SELECT id,provider,username,external_id FROM removed)
    UNION ALL
    (SELECT id,provider,username,external_id FROM removed EXCEPT SELECT id,provider,username,external_id FROM added)
   ) changed_accounts WHERE username IS NOT NULL OR provider='wechat'
 $keys$);
