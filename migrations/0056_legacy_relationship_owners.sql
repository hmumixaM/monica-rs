CREATE TABLE crm.legacy_relationship_owners (
 edge_id uuid PRIMARY KEY REFERENCES crm.relationship_edges(id),
 from_account_id uuid REFERENCES crm.source_accounts(id),
 to_account_id uuid REFERENCES crm.source_accounts(id)
);
CREATE OR REPLACE VIEW crm.presented_relationship_edges AS
 SELECT e.id,
 CASE WHEN o.from_account_id IS NOT NULL THEN a.profile_id ELSE e.from_profile_id END AS from_profile_id,
 CASE WHEN o.to_account_id IS NOT NULL THEN b.profile_id ELSE e.to_profile_id END AS to_profile_id,
 e.relationship_type,e.note,e.valid_from,e.valid_to,e.source_type,e.confidence,e.created_at,e.updated_at
 FROM crm.relationship_edges e LEFT JOIN crm.legacy_relationship_owners o ON o.edge_id=e.id
 LEFT JOIN crm.source_account_links a ON a.account_id=o.from_account_id
 LEFT JOIN crm.source_account_links b ON b.account_id=o.to_account_id
 WHERE (o.from_account_id IS NULL OR a.profile_id IS NOT NULL) AND (o.to_account_id IS NULL OR b.profile_id IS NOT NULL)
 AND (CASE WHEN o.from_account_id IS NOT NULL THEN a.profile_id ELSE e.from_profile_id END)<>(CASE WHEN o.to_account_id IS NOT NULL THEN b.profile_id ELSE e.to_profile_id END)
 UNION ALL
 SELECT e.id,a.profile_id,b.profile_id,e.relationship_type,e.note,e.valid_from,e.valid_to,e.source_type,e.confidence,
 greatest(a.linked_at,b.linked_at),greatest(a.linked_at,b.linked_at)
 FROM crm.source_relationship_edges e JOIN crm.source_account_links a ON a.account_id=e.from_account_id
 JOIN crm.source_account_links b ON b.account_id=e.to_account_id
 WHERE a.profile_id<>b.profile_id AND NOT EXISTS(SELECT 1 FROM crm.relationship_edges m WHERE m.id=e.id);
