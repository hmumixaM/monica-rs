CREATE TABLE crm.timeline_events (
 id uuid PRIMARY KEY,provider text NOT NULL,object_id uuid NOT NULL,source_account_id uuid NOT NULL REFERENCES crm.source_accounts(id),
 profile_id uuid REFERENCES crm.profiles(id),event_type text NOT NULL CHECK(event_type IN ('post','story')),
 external_id text NOT NULL,occurred_at timestamptz,expires_at timestamptz,content jsonb NOT NULL,
 content_hash text NOT NULL,status text NOT NULL DEFAULT 'active',version bigint NOT NULL,
 source_updated_at timestamptz,last_seen_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(provider,object_id)
);
CREATE INDEX ON crm.timeline_events(profile_id,occurred_at DESC,id DESC) WHERE status<>'deleted';
CREATE INDEX ON crm.timeline_events(occurred_at DESC,id DESC) WHERE status<>'deleted';
CREATE TABLE crm.social_interactions (
 id uuid PRIMARY KEY,post_id uuid NOT NULL REFERENCES crm.timeline_events(id),provider text NOT NULL,
 external_id text NOT NULL,interaction_type text NOT NULL,author_account_id uuid REFERENCES crm.source_accounts(id),
 author_profile_id uuid REFERENCES crm.profiles(id),author_external_id text,author_name text,
 occurred_at timestamptz,content jsonb NOT NULL,metadata jsonb NOT NULL DEFAULT '{}',is_deleted boolean NOT NULL DEFAULT false,
 UNIQUE(post_id,external_id,interaction_type)
);
CREATE INDEX ON crm.social_interactions(post_id,occurred_at,id);
CREATE VIEW crm.social_posts AS SELECT id,provider,external_id,profile_id,source_account_id,occurred_at,content,content_hash AS current_content_hash,source_updated_at,last_seen_at,status FROM crm.timeline_events WHERE event_type='post';
CREATE VIEW crm.social_stories AS SELECT id,provider,external_id,profile_id,source_account_id,occurred_at,expires_at,content,content_hash AS current_content_hash,status AS coverage_status FROM crm.timeline_events WHERE event_type='story';
-- Activity module extends this read model with its manual records.
CREATE VIEW crm.timeline_feed AS SELECT id,provider,external_id,profile_id,event_type,occurred_at,content,content_hash,status FROM crm.timeline_events;
-- Incremental per-sender group speech counters; never rescan message bodies for dashboards.
CREATE TABLE crm.conversation_sender_stats (
 conversation_id uuid NOT NULL REFERENCES crm.conversations(id),sender text NOT NULL,
 sender_account_id uuid REFERENCES crm.source_accounts(id),messages bigint NOT NULL,
 PRIMARY KEY(conversation_id,sender)
);
CREATE INDEX ON crm.conversation_sender_stats(sender_account_id,conversation_id) WHERE messages>0;
CREATE FUNCTION crm.update_sender_stats() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_sql text;
BEGIN
 source_sql:=CASE TG_OP WHEN 'INSERT' THEN 'SELECT conversation_id,sender,sender_account_id,1::bigint delta FROM added WHERE NOT tombstone'
 WHEN 'DELETE' THEN 'SELECT conversation_id,sender,sender_account_id,-1::bigint delta FROM removed WHERE NOT tombstone'
 ELSE 'SELECT conversation_id,sender,sender_account_id,1::bigint delta FROM added WHERE NOT tombstone UNION ALL SELECT conversation_id,sender,sender_account_id,-1::bigint delta FROM removed WHERE NOT tombstone' END;
 EXECUTE 'INSERT INTO crm.conversation_sender_stats SELECT conversation_id,sender,(array_agg(sender_account_id) FILTER(WHERE sender_account_id IS NOT NULL))[1],sum(delta) FROM ('||source_sql||') changes GROUP BY conversation_id,sender ORDER BY conversation_id,sender ON CONFLICT(conversation_id,sender) DO UPDATE SET messages=crm.conversation_sender_stats.messages+excluded.messages,sender_account_id=coalesce(excluded.sender_account_id,crm.conversation_sender_stats.sender_account_id)';
 RETURN NULL;
END $$;
CREATE TRIGGER sender_stats_insert AFTER INSERT ON crm.message_search REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.update_sender_stats();
CREATE TRIGGER sender_stats_update AFTER UPDATE ON crm.message_search REFERENCING OLD TABLE AS removed NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION crm.update_sender_stats();
CREATE TRIGGER sender_stats_delete AFTER DELETE ON crm.message_search REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION crm.update_sender_stats();
INSERT INTO crm.conversation_sender_stats SELECT conversation_id,sender,(array_agg(sender_account_id) FILTER(WHERE sender_account_id IS NOT NULL))[1],count(*) FROM crm.message_search WHERE NOT tombstone GROUP BY conversation_id,sender;
