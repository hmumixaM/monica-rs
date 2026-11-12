-- Substring search is intentional (including CJK), so index current searchable
-- text with trigram operators instead of replaying historical JSON payloads.
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA crm;
CREATE INDEX page_post_text_search ON crm.page_posts USING gin(text crm.gin_trgm_ops);
CREATE INDEX page_profile_name_search ON crm.profiles USING gin(display_name crm.gin_trgm_ops) WHERE archived_at IS NULL;
CREATE INDEX page_profile_summary_search ON crm.profiles USING gin(summary crm.gin_trgm_ops) WHERE archived_at IS NULL;
CREATE INDEX page_tag_search ON crm.tags USING gin(name crm.gin_trgm_ops);
CREATE INDEX page_activity_title_search ON crm.activities USING gin(title crm.gin_trgm_ops);
CREATE INDEX page_activity_body_search ON crm.activities USING gin(body crm.gin_trgm_ops);
CREATE INDEX page_message_time ON crm.message_search(occurred_at,conversation_id) WHERE NOT tombstone;
CREATE INDEX page_posts_location_time ON crm.page_posts(occurred_at,profile_id) WHERE location IS NOT NULL AND location<>'null'::jsonb;
