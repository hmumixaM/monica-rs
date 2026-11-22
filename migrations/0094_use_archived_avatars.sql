-- People kept their initials although a linked WeChat, Instagram or LinkedIn
-- account had an avatar: only avatars whose bytes were checked at import were
-- eligible, and the archive copied before that check existed. Give those
-- people the avatar the application now chooses for them. A hand-picked
-- avatar, or one already chosen from a source, is left alone.
WITH chosen AS (
  SELECT DISTINCT ON (l.profile_id) l.profile_id, m.media_id, l.account_id
  FROM crm.source_account_links l
  JOIN crm.profiles p ON p.id=l.profile_id AND p.avatar_media_id IS NULL
  JOIN crm.media_slots m ON m.entity_type='source_account' AND m.entity_id=l.account_id
    AND m.role IN ('wechat_avatar_small','wechat_avatar','wechat_avatar_big','instagram_avatar','linkedin_avatar')
  JOIN crm.media_assets a ON a.id=m.media_id
  WHERE NOT EXISTS (SELECT 1 FROM crm.media_slots manual
      WHERE manual.entity_type='profile' AND manual.entity_id=l.profile_id AND manual.role='avatar')
    AND NOT EXISTS (SELECT 1 FROM crm.source_avatar_projections s WHERE s.profile_id=l.profile_id)
  ORDER BY l.profile_id, (a.verified_at IS NULL),
    CASE m.role WHEN 'wechat_avatar_small' THEN 0 WHEN 'wechat_avatar' THEN 1 WHEN 'wechat_avatar_big' THEN 2
      WHEN 'instagram_avatar' THEN 3 ELSE 4 END, l.account_id
), shown AS (
  UPDATE crm.profiles p SET avatar_media_id=c.media_id,generation=generation+1
  FROM chosen c WHERE p.id=c.profile_id
)
INSERT INTO crm.source_avatar_projections(profile_id,media_id,source_account_id)
SELECT profile_id,media_id,account_id FROM chosen;
