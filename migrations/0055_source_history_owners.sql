CREATE OR REPLACE FUNCTION crm.page_entity_count() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;scope text=TG_ARGV[1];metric text=TG_ARGV[0];predicate text=coalesce(nullif(TG_ARGV[2],''),'true');
BEGIN
 source_sql := CASE TG_OP WHEN 'INSERT' THEN format('SELECT %I subject,1::bigint delta FROM added WHERE %s',scope,predicate)
 WHEN 'DELETE' THEN format('SELECT %I subject,-1::bigint delta FROM removed WHERE %s',scope,predicate)
 ELSE format('SELECT %I subject,1::bigint delta FROM added WHERE %s UNION ALL SELECT %I subject,-1::bigint delta FROM removed WHERE %s',scope,predicate,scope,predicate) END;
 EXECUTE 'INSERT INTO crm.page_entity_counts SELECT $1,subject,sum(delta) FROM ('||source_sql||') x WHERE subject IS NOT NULL GROUP BY subject ORDER BY subject ON CONFLICT(metric,subject) DO UPDATE SET value=crm.page_entity_counts.value+excluded.value' USING metric;
 RETURN NULL;
END $$;
CREATE TABLE crm.source_account_tags (
 account_id uuid NOT NULL REFERENCES crm.source_accounts(id),tag_id uuid NOT NULL REFERENCES crm.tags(id),
 PRIMARY KEY(account_id,tag_id)
);
CREATE VIEW crm.presented_tag_memberships AS
 SELECT DISTINCT ON(tag_id,profile_id) tag_id,profile_id,source_type FROM (
 SELECT tag_id,profile_id,source_type FROM crm.tag_memberships
 UNION ALL
 SELECT t.tag_id,l.profile_id,a.provider FROM crm.source_account_tags t
 JOIN crm.source_account_links l ON l.account_id=t.account_id JOIN crm.source_accounts a ON a.id=t.account_id
 WHERE NOT EXISTS(SELECT 1 FROM crm.tag_membership_removals r WHERE r.profile_id=l.profile_id AND r.tag_id=t.tag_id)
 ) memberships ORDER BY tag_id,profile_id,(source_type='manual') DESC,source_type;
