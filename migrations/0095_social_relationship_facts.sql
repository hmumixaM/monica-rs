-- Relationship facts that the sources already deliver but nothing recorded.
--
-- LinkedIn: a contact's captured list of mutual connections says which of the
-- people in the address book are connected to each other.
-- Instagram: each post lists who liked it and who is tagged or mentioned, and
-- each story lists who it mentions. These become interactions, as WeChat
-- Moments likes and mentions already are, so the relationship graph and the
-- post's own list of interactions can read them.

CREATE FUNCTION crm.linkedin_slug(url text) RETURNS text LANGUAGE sql IMMUTABLE AS $$
  SELECT nullif(lower(split_part(split_part(regexp_replace(url, '^.*/in/', ''), '?', 1), '/', 1)), '')
$$;

-- One edge for each pair, whichever of the two listed the other. A list that
-- was captured in full also retires the pairs it no longer contains.
CREATE FUNCTION crm.record_linkedin_mutuals(accounts uuid[]) RETURNS void LANGUAGE sql AS $$
  WITH lists AS (
    SELECT f.account_id, f.value FROM crm.source_fields f
    JOIN crm.source_accounts a ON a.id=f.account_id AND a.provider='linkedin'
    WHERE f.field_key='linkedin.mutuals' AND f.account_id=ANY(accounts)
  ), listed AS (
    SELECT DISTINCT l.account_id, b.id other FROM lists l
    CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(l.value->'connections')='array' THEN l.value->'connections' ELSE '[]'::jsonb END) c
    JOIN crm.source_accounts b ON b.provider='linkedin' AND lower(b.external_id)=crm.linkedin_slug(c->>'profile_url') AND b.id<>l.account_id
  ), retired AS (
    DELETE FROM crm.source_relationship_edges e USING lists l
    WHERE e.source_type='linkedin' AND e.relationship_type='linkedin_connection'
      AND l.account_id IN (e.from_account_id,e.to_account_id) AND l.value->>'is_complete'='true'
      AND NOT EXISTS (SELECT 1 FROM listed k WHERE k.account_id=l.account_id AND k.other IN (e.from_account_id,e.to_account_id))
      -- The other person's own list may still say they are connected.
      AND NOT EXISTS (SELECT 1 FROM crm.source_fields f
        CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(f.value->'connections')='array' THEN f.value->'connections' ELSE '[]'::jsonb END) c
        JOIN crm.source_accounts mine ON mine.id=l.account_id
        WHERE f.field_key='linkedin.mutuals' AND f.account_id<>l.account_id AND f.account_id IN (e.from_account_id,e.to_account_id)
          AND crm.linkedin_slug(c->>'profile_url')=lower(mine.external_id))
  )
  INSERT INTO crm.source_relationship_edges(id,from_account_id,to_account_id,relationship_type,confidence,source_type,evidence_kind)
  SELECT DISTINCT md5('linkedin_connection:'||least(account_id,other)||':'||greatest(account_id,other))::uuid,
    least(account_id,other),greatest(account_id,other),'linkedin_connection',1,'linkedin','relationship'
  FROM listed ON CONFLICT(id) DO NOTHING
$$;

CREATE FUNCTION crm.linkedin_mutuals_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM crm.record_linkedin_mutuals(ARRAY[NEW.account_id]);
  RETURN NULL;
END $$;
CREATE TRIGGER linkedin_mutuals AFTER INSERT OR UPDATE OF value ON crm.source_fields
  FOR EACH ROW WHEN (NEW.field_key='linkedin.mutuals') EXECUTE FUNCTION crm.linkedin_mutuals_changed();

-- A person who joins the address book later may already be on other lists.
CREATE FUNCTION crm.linkedin_account_added() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM crm.record_linkedin_mutuals(array_agg(f.account_id)) FROM crm.source_fields f
  WHERE f.field_key='linkedin.mutuals' AND f.account_id<>NEW.id AND EXISTS (
    SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(f.value->'connections')='array' THEN f.value->'connections' ELSE '[]'::jsonb END) c
    WHERE crm.linkedin_slug(c->>'profile_url')=lower(NEW.external_id))
  HAVING count(*)>0;
  RETURN NULL;
END $$;
CREATE TRIGGER linkedin_account_added AFTER INSERT ON crm.source_accounts
  FOR EACH ROW WHEN (NEW.provider='linkedin') EXECUTE FUNCTION crm.linkedin_account_added();

SELECT crm.record_linkedin_mutuals(array_agg(account_id)) FROM crm.source_fields WHERE field_key='linkedin.mutuals' HAVING count(*)>0;

-- Instagram. Derived rows are named like:ID, tag:ID, caption:NAME and
-- mention:ID; comments keep the ids Instagram gave them. A mention is written
-- by the owner of the post or story and names its target, as in WeChat.
CREATE FUNCTION crm.record_instagram_interactions(events uuid[]) RETURNS void LANGUAGE sql AS $$
  WITH posts AS (
    SELECT e.id,e.occurred_at,e.content,o.external_id owner,coalesce(o.username,o.display_name) owner_name
    FROM crm.timeline_events e JOIN crm.source_accounts o ON o.id=e.source_account_id
    WHERE e.id=ANY(events) AND e.provider='instagram'
  ), facts AS (
    SELECT p.id post_id,p.occurred_at,'like' kind,'like:'||(u->>'external_id') external_id,u->>'external_id' author,u->>'username' author_name,
      NULL::text target,jsonb_build_object('username',u->>'username','name',u->>'name') content
    FROM posts p CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(p.content#>'{people_preview,likers}')='array' THEN p.content#>'{people_preview,likers}' ELSE '[]'::jsonb END) u
    WHERE u->>'external_id' IS NOT NULL AND u->>'external_id'<>p.owner
    UNION ALL
    SELECT p.id,p.occurred_at,'mention','tag:'||(u->>'external_id'),p.owner,p.owner_name,u->>'external_id',
      jsonb_build_object('username',u->>'username','name',u->>'name','via','tag')
    FROM posts p CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(p.content->'tagged_users')='array' THEN p.content->'tagged_users' ELSE '[]'::jsonb END) u
    WHERE u->>'external_id' IS NOT NULL AND u->>'external_id'<>p.owner
    UNION ALL
    -- Someone named in the caption and not tagged. The caption has only the
    -- username, so the person is found through the accounts already known.
    SELECT p.id,p.occurred_at,'mention','caption:'||lower(u->>'username'),p.owner,p.owner_name,
      coalesce(known.external_id,'username:'||lower(u->>'username')),jsonb_build_object('username',u->>'username','via','caption')
    FROM posts p CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(p.content->'caption_mentions')='array' THEN p.content->'caption_mentions' ELSE '[]'::jsonb END) u
    LEFT JOIN LATERAL (SELECT a.external_id FROM crm.source_accounts a WHERE a.provider='instagram' AND lower(a.username)=lower(u->>'username') ORDER BY a.id LIMIT 1) known ON true
    WHERE u->>'username' IS NOT NULL AND lower(u->>'username')<>lower(p.owner_name) AND known.external_id IS DISTINCT FROM p.owner
      AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(p.content->'tagged_users')='array' THEN p.content->'tagged_users' ELSE '[]'::jsonb END) t WHERE lower(t->>'username')=lower(u->>'username'))
    UNION ALL
    SELECT p.id,p.occurred_at,'mention','mention:'||(u#>>'{user,pk}'),p.owner,p.owner_name,u#>>'{user,pk}',
      jsonb_build_object('username',u#>>'{user,username}','name',u#>>'{user,full_name}','via','story')
    FROM posts p CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(p.content#>'{archive_manifest,raw_metadata,iphone_struct,reel_mentions}')='array' THEN p.content#>'{archive_manifest,raw_metadata,iphone_struct,reel_mentions}' ELSE '[]'::jsonb END) u
    WHERE u#>>'{user,pk}' IS NOT NULL AND u#>>'{user,pk}'<>p.owner
  ), kept AS (
    INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_account_id,author_profile_id,author_external_id,author_name,occurred_at,content,metadata,is_deleted)
    SELECT DISTINCT ON (f.post_id,f.external_id,f.kind) md5(f.post_id||':'||f.kind||':'||f.external_id)::uuid,f.post_id,'instagram',f.external_id,f.kind,
      a.id,l.profile_id,f.author,f.author_name,f.occurred_at,f.content,
      CASE WHEN f.target IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('target_external_id',f.target) END,false
    FROM facts f LEFT JOIN crm.source_accounts a ON a.provider='instagram' AND a.external_id=f.author
    LEFT JOIN crm.source_account_links l ON l.account_id=a.id
    ORDER BY f.post_id,f.external_id,f.kind
    ON CONFLICT(post_id,external_id,interaction_type) DO UPDATE SET author_account_id=excluded.author_account_id,author_profile_id=excluded.author_profile_id,
      author_name=excluded.author_name,content=excluded.content,metadata=excluded.metadata,is_deleted=false
    RETURNING post_id,external_id,interaction_type
  )
  UPDATE crm.social_interactions s SET is_deleted=true
  WHERE s.post_id=ANY(events) AND s.provider='instagram' AND NOT s.is_deleted AND s.external_id ~ '^(like|tag|caption|mention):'
    AND NOT EXISTS (SELECT 1 FROM kept k WHERE k.post_id=s.post_id AND k.external_id=s.external_id AND k.interaction_type=s.interaction_type)
$$;

CREATE FUNCTION crm.instagram_post_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' OR NEW.content IS DISTINCT FROM OLD.content OR NEW.source_account_id IS DISTINCT FROM OLD.source_account_id THEN
    PERFORM crm.record_instagram_interactions(ARRAY[NEW.id]);
  END IF;
  RETURN NULL;
END $$;
CREATE TRIGGER instagram_interactions AFTER INSERT OR UPDATE OF content,source_account_id ON crm.timeline_events
  FOR EACH ROW WHEN (NEW.provider='instagram') EXECUTE FUNCTION crm.instagram_post_changed();

SELECT crm.record_instagram_interactions(array_agg(id)) FROM crm.timeline_events WHERE provider='instagram' HAVING count(*)>0;
