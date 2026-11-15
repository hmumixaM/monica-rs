-- Legacy Monica contact IDs identify notebook provenance, not a second
-- external person account. Keep those IDs and all historical records while
-- applying the same official-account policy to migrated and native people.
CREATE OR REPLACE VIEW crm.directory_profiles AS
 SELECT p.* FROM crm.profiles p WHERE p.archived_at IS NULL AND (
  p.profile_type<>'person'
  OR NOT EXISTS(SELECT 1 FROM crm.profile_directory_accounts a WHERE a.profile_id=p.id AND a.provider='wechat' AND crm.wechat_is_official(a.external_id))
  OR EXISTS(SELECT 1 FROM crm.profile_directory_accounts a WHERE a.profile_id=p.id AND a.provider<>'monica' AND NOT(a.provider='wechat' AND crm.wechat_is_official(a.external_id)))
  OR EXISTS(SELECT 1 FROM crm.audit_events e WHERE e.entity_id=p.id AND e.entity_type='profile' AND e.action='profile.created')
 );

-- Cached directory, graph and summary reads must observe the corrected view.
INSERT INTO crm.cache_invalidations
 SELECT DISTINCT pg_current_xact_id(),'profile',a.profile_id FROM crm.profile_directory_accounts a
 WHERE a.provider='wechat' AND crm.wechat_is_official(a.external_id) ORDER BY 3 ON CONFLICT DO NOTHING;
