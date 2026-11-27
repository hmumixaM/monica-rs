WITH accounts AS MATERIALIZED (
    SELECT id,provider,external_id FROM crm.source_accounts WHERE lower(username)=lower($4)
    UNION
    SELECT id,provider,external_id FROM crm.source_accounts
    WHERE provider='wechat' AND external_id IN ($4,lower($4))
), people AS MATERIALIZED (
    SELECT i.profile_id FROM crm.identities i
    WHERE lower(i.username)=lower($4) AND i.status NOT IN ('invalid','merged') AND i.profile_id IS NOT NULL
    UNION
    SELECT i.profile_id FROM crm.identities i
    WHERE i.provider='wechat' AND i.external_id IN ($4,lower($4))
      AND i.status NOT IN ('invalid','merged') AND i.profile_id IS NOT NULL
    UNION
    SELECT l.profile_id FROM accounts a JOIN crm.source_account_links l ON l.account_id=a.id
    UNION
    SELECT c.profile_id FROM crm.field_revisions r JOIN crm.field_current c
      ON c.revision_id=r.id AND c.profile_id=r.profile_id AND c.field_key=r.field_key
    WHERE r.field_key IN ('wechat.alias','wechat.nickname','wechat.remark')
      AND jsonb_typeof(r.value)='string' AND lower(r.value #>> '{}')=lower($4)
), wechat_targets AS (
    SELECT $4 AS external_id UNION SELECT lower($4)
    UNION SELECT external_id FROM accounts WHERE provider='wechat'
    UNION
    SELECT i.external_id FROM people p JOIN crm.identities i ON i.profile_id=p.profile_id
    WHERE i.provider='wechat' AND i.status NOT IN ('invalid','merged')
    UNION
    SELECT a.external_id FROM people p JOIN crm.source_account_links l ON l.profile_id=p.profile_id
    JOIN crm.source_accounts a ON a.id=l.account_id AND a.provider='wechat'
), candidates AS (
    (SELECT id,1 AS priority,'caption' AS source,NULL::uuid AS media_id
     FROM crm.page_posts WHERE text ILIKE $1 ORDER BY occurred_at DESC NULLS LAST,id LIMIT $3)
    UNION ALL
    SELECT e.id,4,'account',NULL FROM accounts a JOIN crm.timeline_events e ON e.source_account_id=a.id
    UNION ALL
    SELECT x.id,4,'account',NULL FROM people p JOIN crm.page_posts x ON x.profile_id=p.profile_id
    UNION ALL
    SELECT id,4,'account',NULL FROM crm.timeline_events
    WHERE provider='instagram' AND status NOT IN ('deleted','merged') AND lower(content->>'username')=lower($4)
    UNION ALL
    SELECT id,3,'mention',NULL FROM crm.timeline_events
    WHERE provider='instagram' AND status NOT IN ('deleted','merged')
      AND crm.instagram_search_mentions(content) @> ARRAY[lower($4)]
    UNION ALL
    SELECT s.post_id,3,'mention',NULL FROM wechat_targets t JOIN crm.social_interactions s
      ON coalesce(s.content->>'target_external_id',s.metadata->>'target_external_id')=t.external_id
    WHERE s.provider='wechat' AND s.interaction_type='mention' AND NOT s.is_deleted
    UNION ALL
    SELECT l.entity_id,2,'ocr',r.media_id
    FROM crm.image_ocr_readings r JOIN crm.media_links l ON l.media_id=r.media_id
    WHERE l.entity_type IN ('social_post','social_story','post','story')
      AND (r.status='complete' OR r.generated_at IS NOT NULL)
      AND crm.search_grams(r.text) @@ crm.search_gram_query($5) AND r.text ILIKE $1
    UNION ALL
    SELECT r.entity_id,2,'legacy_ocr',NULL FROM crm.media_readings r
    WHERE (r.status='complete' OR r.generated_at IS NOT NULL)
      AND crm.search_grams(crm.post_reading_search_text(r.items)) @@ crm.search_gram_query($5)
      AND crm.post_reading_search_text(r.items) ILIKE $1
), matched AS (
    SELECT DISTINCT ON (c.id) c.id,c.source,c.media_id,x.occurred_at
    FROM candidates c JOIN crm.page_posts x ON x.id=c.id
    ORDER BY c.id,c.priority,c.source,c.media_id
), post_page AS (
    SELECT * FROM matched ORDER BY occurred_at DESC NULLS LAST,id LIMIT $3
)
SELECT to_jsonb(hits) FROM (
    (SELECT p.id,'profile' entity_type,p.display_name title,coalesce(p.summary,'') summary,NULL::timestamptz occurred_at,
        p.id profile_id,p.display_name profile_name,p.profile_type kind,p.avatar_media_id,NULL::text provider,NULL::text match_source
     FROM crm.directory_profiles p WHERE p.archived_at IS NULL AND (p.display_name ILIKE $1 OR p.summary ILIKE $1)
     ORDER BY p.display_name ILIKE $2 DESC,length(p.display_name),p.display_name LIMIT $3)
    UNION ALL
    (SELECT t.id,'tag',t.name,'',NULL,NULL,NULL,NULL,NULL,NULL,NULL FROM crm.tags t
     WHERE t.name ILIKE $1 ORDER BY length(t.name),t.name LIMIT $3)
    UNION ALL
    (SELECT x.id,'timeline',coalesce(p.display_name,x.provider),
        substring(detail.text FROM greatest(strpos(lower(detail.text),lower($5))-70,1) FOR 240),
        x.occurred_at,x.profile_id,p.display_name,x.kind,p.avatar_media_id,x.provider,
        CASE WHEN hit.source='legacy_ocr' THEN 'ocr' ELSE hit.source END
     FROM post_page hit JOIN crm.page_posts x ON x.id=hit.id
     LEFT JOIN crm.directory_profiles p ON p.id=x.profile_id
     LEFT JOIN crm.image_ocr_readings o ON o.media_id=hit.media_id
     LEFT JOIN crm.media_readings saved ON hit.source='legacy_ocr' AND saved.entity_id=x.id
        AND saved.entity_type=CASE WHEN x.kind='story' THEN 'social_story' ELSE 'social_post' END
     CROSS JOIN LATERAL (
        SELECT coalesce(o.text,crm.post_reading_search_text(saved.items),x.text) AS text
     ) detail
     ORDER BY x.occurred_at DESC NULLS LAST,x.id)
    UNION ALL
    (SELECT a.id,'activity',a.title,coalesce(a.body,''),a.occurred_at,p.id,p.display_name,a.activity_type,NULL,a.source_type,NULL
     FROM crm.activities a LEFT JOIN LATERAL (
         SELECT p.id,p.display_name FROM crm.activity_participants ap JOIN crm.profiles p ON p.id=ap.profile_id
         WHERE ap.activity_id=a.id ORDER BY p.display_name,p.id LIMIT 1
     ) p ON true
     WHERE a.activity_type NOT IN ('wechat_direct_summary','wechat_group_summary') AND (a.title ILIKE $1 OR a.body ILIKE $1)
     ORDER BY a.occurred_at DESC,a.id LIMIT $3)
) hits
