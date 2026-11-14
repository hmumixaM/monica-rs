-- Keep legacy evidence categories without projecting shares/interactions as
-- explicit person relationships. Manual writes retain the relationship default.
ALTER TABLE crm.relationship_edges ADD COLUMN evidence_kind text NOT NULL DEFAULT 'relationship';
UPDATE crm.relationship_edges e SET evidence_kind=h.payload->>'evidence_kind'
 FROM crm.legacy_history h WHERE h.store='crm' AND h.table_name='relationship_edges'
 AND h.payload->>'id'=e.id::text AND h.payload->>'evidence_kind' IS NOT NULL;
DROP INDEX crm.relationship_manual_pair;
CREATE UNIQUE INDEX relationship_manual_pair ON crm.relationship_edges(from_profile_id,to_profile_id,relationship_type)
 WHERE NOT legacy_source_owned AND evidence_kind='relationship';

CREATE OR REPLACE VIEW crm.presented_relationship_edges AS
 WITH candidates AS (
 SELECT e.id,
 CASE WHEN o.from_account_id IS NOT NULL THEN a.profile_id ELSE e.from_profile_id END AS from_profile_id,
 CASE WHEN o.to_account_id IS NOT NULL THEN b.profile_id ELSE e.to_profile_id END AS to_profile_id,
 e.relationship_type,e.note,e.valid_from,e.valid_to,e.source_type,e.confidence,e.created_at,e.updated_at,
 NOT e.legacy_source_owned AS manual_priority
 FROM crm.relationship_edges e LEFT JOIN crm.legacy_relationship_owners o ON o.edge_id=e.id
 LEFT JOIN crm.source_account_links a ON a.account_id=o.from_account_id
 LEFT JOIN crm.source_account_links b ON b.account_id=o.to_account_id
 WHERE e.evidence_kind='relationship' AND (o.from_account_id IS NULL OR a.profile_id IS NOT NULL) AND (o.to_account_id IS NULL OR b.profile_id IS NOT NULL)
 AND (CASE WHEN o.from_account_id IS NOT NULL THEN a.profile_id ELSE e.from_profile_id END)<>(CASE WHEN o.to_account_id IS NOT NULL THEN b.profile_id ELSE e.to_profile_id END)
 UNION ALL
 SELECT e.id,a.profile_id,b.profile_id,e.relationship_type,e.note,e.valid_from,e.valid_to,e.source_type,e.confidence,
 greatest(a.linked_at,b.linked_at),greatest(a.linked_at,b.linked_at),false
 FROM crm.source_relationship_edges e JOIN crm.source_account_links a ON a.account_id=e.from_account_id
 JOIN crm.source_account_links b ON b.account_id=e.to_account_id
 WHERE e.evidence_kind='relationship' AND a.profile_id<>b.profile_id AND NOT EXISTS(SELECT 1 FROM crm.relationship_edges m WHERE m.id=e.id)
 )
 SELECT DISTINCT ON(from_profile_id,to_profile_id,relationship_type)
 id,from_profile_id,to_profile_id,relationship_type,note,valid_from,valid_to,source_type,confidence,created_at,updated_at
 FROM candidates ORDER BY from_profile_id,to_profile_id,relationship_type,manual_priority DESC,updated_at DESC,id;
