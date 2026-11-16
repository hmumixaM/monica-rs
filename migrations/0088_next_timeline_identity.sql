-- Keep Monica Next IDs when a source importer encounters the same post.
CREATE INDEX timeline_unbound_source_identity
 ON crm.timeline_events(provider,event_type,external_id,profile_id)
 WHERE source_account_id IS NULL AND connection_id IS NULL AND status<>'merged';

CREATE TABLE crm.timeline_event_aliases (
 alias_id uuid PRIMARY KEY,
 event_id uuid NOT NULL REFERENCES crm.timeline_events(id),
 original_row jsonb NOT NULL,
 preserved_data jsonb NOT NULL DEFAULT '{}',
 merged_at timestamptz NOT NULL DEFAULT now(),
 CHECK(alias_id<>event_id)
);
CREATE TABLE crm.social_interaction_aliases (
 alias_id uuid PRIMARY KEY,
 interaction_id uuid NOT NULL REFERENCES crm.social_interactions(id),
 original_row jsonb NOT NULL,
 merged_at timestamptz NOT NULL DEFAULT now(),
 CHECK(alias_id<>interaction_id)
);

CREATE FUNCTION crm.adopt_legacy_timeline_identity(
 source_provider text, source_kind text, source_external text,
 source_account uuid, owner uuid, source_object uuid
) RETURNS uuid LANGUAGE plpgsql AS $$
DECLARE existing uuid; candidates uuid[]; previous_row jsonb;
BEGIN
 SELECT id INTO existing FROM crm.timeline_events
 WHERE provider=source_provider AND object_id=source_object;
 IF existing IS NOT NULL THEN RETURN existing; END IF;
 -- The author account and source post ID establish identity. A matching
 -- caption or timestamp alone must never join two different source posts.
 SELECT array_agg(id) INTO candidates FROM (
  SELECT e.id FROM crm.timeline_events e JOIN crm.source_accounts a ON a.id=source_account
  WHERE e.provider=source_provider AND e.event_type=source_kind
   AND e.external_id=source_external AND (e.profile_id=owner OR source_provider='wechat')
   AND e.source_account_id IS NULL AND e.connection_id IS NULL AND e.status<>'merged'
   AND e.content->>'username'=a.external_id AND a.provider=source_provider
  ORDER BY e.id FOR UPDATE OF e
 ) matched;
 IF cardinality(candidates)=1 THEN
  SELECT to_jsonb(e) INTO previous_row FROM crm.timeline_events e WHERE id=candidates[1];
  UPDATE crm.timeline_events SET source_account_id=source_account,object_id=source_object
  WHERE id=candidates[1];
  INSERT INTO crm.legacy_chatlog_projection(entity_type,entity_id,original_row,current_row,revision)
  VALUES('post',candidates[1],jsonb_build_object('source_account_id',previous_row->'source_account_id','object_id',previous_row->'object_id'),
   jsonb_build_object('source_account_id',source_account,'object_id',source_object),(previous_row->>'version')::bigint)
  ON CONFLICT(entity_type,entity_id) DO UPDATE
   SET original_row=excluded.original_row||crm.legacy_chatlog_projection.original_row,
    current_row=crm.legacy_chatlog_projection.current_row||excluded.current_row,
    revision=greatest(crm.legacy_chatlog_projection.revision,excluded.revision);
  RETURN candidates[1];
 END IF;
 RETURN NULL;
END $$;

-- Archived projections retain immutable revision foreign keys. Only canonical
-- posts enter the active read models; their original IDs resolve via aliases.
CREATE OR REPLACE VIEW crm.social_posts AS
 SELECT id,provider,external_id,profile_id,source_account_id,occurred_at,content,
 content_hash AS current_content_hash,source_updated_at,last_seen_at,status
 FROM crm.timeline_events WHERE event_type='post' AND status<>'merged';
CREATE OR REPLACE VIEW crm.social_stories AS
 SELECT id,provider,external_id,profile_id,source_account_id,occurred_at,expires_at,content,
 content_hash AS current_content_hash,status AS coverage_status
 FROM crm.timeline_events WHERE event_type='story' AND status<>'merged';
CREATE OR REPLACE VIEW crm.timeline_feed AS
 SELECT id,provider,external_id,profile_id,event_type,occurred_at,content,content_hash,status
 FROM crm.timeline_events WHERE status<>'merged'
 UNION ALL
 SELECT a.id,'manual',a.id::text,NULL::uuid,'activity',a.occurred_at,
 to_jsonb(a)-'content_hash'||jsonb_build_object('text',a.body),a.content_hash,'active'
 FROM crm.activities a WHERE activity_type NOT IN ('wechat_direct_summary','wechat_group_summary');

CREATE OR REPLACE FUNCTION crm.page_post_current() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.status IN ('deleted','merged') THEN DELETE FROM crm.page_posts WHERE id=NEW.id;
 ELSE INSERT INTO crm.page_posts(id,profile_id,provider,kind,occurred_at,text,location,coordinate_format,generation,media_count)
 VALUES(NEW.id,NEW.profile_id,NEW.provider,NEW.event_type,NEW.occurred_at,
 coalesce(NEW.content->>'text',NEW.content->>'caption',NEW.content->>'summary',NEW.content->>'body',''),
 NEW.content->'location',NEW.content->>'coordinate_format',NEW.version,
 CASE WHEN jsonb_typeof(NEW.content->'media')='array' THEN jsonb_array_length(NEW.content->'media') ELSE 0 END)
 ON CONFLICT(id) DO UPDATE SET profile_id=excluded.profile_id,provider=excluded.provider,kind=excluded.kind,occurred_at=excluded.occurred_at,text=excluded.text,location=excluded.location,coordinate_format=excluded.coordinate_format,generation=excluded.generation,media_count=excluded.media_count;
 END IF; RETURN NULL;
END $$;
