-- Account matching must not rescan the complete media-link table for every
-- account. This also supports a prebuilt concurrent index on an active NAS.
CREATE INDEX IF NOT EXISTS media_links_account_avatar
 ON crm.media_links(source_account_id,role,media_id)
 WHERE source_account_id IS NOT NULL
 AND role IN ('wechat_avatar_small','wechat_avatar','instagram_avatar','linkedin_avatar');
