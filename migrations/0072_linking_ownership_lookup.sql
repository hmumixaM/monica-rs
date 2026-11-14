-- The account review snapshot checks these ownership blockers for each account.
-- Keep each lookup local to its account/person instead of repeating table scans.
CREATE INDEX IF NOT EXISTS identities_account_status
 ON crm.identities(source_account_id,status) WHERE source_account_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS tag_memberships_profile_source
 ON crm.tag_memberships(profile_id,source_type);
CREATE INDEX IF NOT EXISTS relationship_edges_from_source
 ON crm.relationship_edges(from_profile_id,source_type,id);
CREATE INDEX IF NOT EXISTS relationship_edges_to_source
 ON crm.relationship_edges(to_profile_id,source_type,id);
